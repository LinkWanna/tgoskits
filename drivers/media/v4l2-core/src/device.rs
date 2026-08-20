//! 设备抽象层——对应 Linux 的 `struct video_device`。
//!
//! `VideoDevice` 将 `IoctlOps` 实现（ioctl 分发、
//! mmap、poll、生命周期）与 `IoctlDispatcher` 捆绑在一起。它是
//! 在 StarryOS 的 pseudofs 中注册为 `/dev/videoX` 节点的单元。

use alloc::{sync::Arc, vec::Vec};

use ax_sync::Mutex;
use axpoll::{IoEvents, PollSet};

use crate::{
    Result, V4l2Error,
    filehandler::V4l2Fh,
    interface::{
        ctrl::{Control, ExtControl, ExtControls},
        event::{Event, EventSubscription},
    },
    ioctl::{IoctlDispatcher, IoctlOps},
};

/// V4L2 视频设备——对应 Linux 的 `struct video_device`。
///
/// 保存向用户空间提供 ioctl 与 VFS 操作所需的全部内容，
/// 服务于单个 `/dev/videoX`。
pub struct VideoDevice {
    driver: Arc<Mutex<dyn IoctlOps>>,
    dispatcher: IoctlDispatcher,
    name: &'static str,
    fh: Option<V4l2Fh>,
    open_count: u32,
    prio: u32,
    event_poll_rx: Arc<PollSet>,
}

impl VideoDevice {
    /// 创建新的视频设备。
    pub fn new(driver: Arc<Mutex<dyn IoctlOps>>, name: &'static str) -> Self {
        Self {
            driver,
            dispatcher: IoctlDispatcher::new(),
            name,
            fh: None,
            open_count: 0,
            prio: 2, // V4L2_PRIORITY_DEFAULT = V4L2_PRIORITY_INTERACTIVE
            event_poll_rx: Arc::new(PollSet::new()),
        }
    }

    /// 获取设备名。
    pub fn name(&self) -> &str {
        self.name
    }

    /// 处理来自用户空间的 ioctl。
    pub fn handle_ioctl(&mut self, cmd: u32, arg: &mut [u8]) -> Result<()> {
        // 拦截事件 ioctl——它们作用于 fh，并路由到驱动回调（Linux
        // `vidioc_subscribe_event`/`vidioc_dqevent`）。
        use crate::ioctl::IoctlCmd;
        if let Some(c) = IoctlCmd::try_from_u32(cmd) {
            match c {
                IoctlCmd::SubscribeEvent => {
                    let sub: EventSubscription = unsafe { crate::ioctl::read_from_bytes(arg) };
                    let mut driver = self.driver.lock();
                    let fh = self.fh.as_mut().ok_or(V4l2Error::BadFileDescriptor)?;
                    driver.subscribe_event(fh, &sub)?;
                    // SEND_INITIAL 的初始事件由驱动直接排入 fh，此处补唤醒
                    // poll POLLPRI 等待者（对齐 Linux 入队后 wake_up_all）。
                    if self.has_pending_events() {
                        self.event_poll_rx.wake_from_irq(IoEvents::PRI);
                    }
                    return Ok(());
                }
                IoctlCmd::UnsubscribeEvent => {
                    let sub: EventSubscription = unsafe { crate::ioctl::read_from_bytes(arg) };
                    let mut driver = self.driver.lock();
                    let fh = self.fh.as_mut().ok_or(V4l2Error::BadFileDescriptor)?;
                    driver.unsubscribe_event(fh, &sub)?;
                    return Ok(());
                }
                IoctlCmd::DQEvent => {
                    let mut ev: Event = unsafe { crate::ioctl::read_from_bytes(arg) };
                    let mut driver = self.driver.lock();
                    let fh = self.fh.as_mut().ok_or(V4l2Error::BadFileDescriptor)?;
                    driver.dqevent(fh, &mut ev)?;
                    unsafe { crate::ioctl::write_to_bytes(arg, &ev) };
                    return Ok(());
                }
                // 优先级由 core 层维护（Linux `v4l2_prio`），不进驱动分发。
                IoctlCmd::GPriority => {
                    let p = self.prio;
                    unsafe { crate::ioctl::write_to_bytes(arg, &p) };
                    return Ok(());
                }
                IoctlCmd::SPriority => {
                    let p: u32 = unsafe { crate::ioctl::read_from_bytes(arg) };
                    // V4L2_PRIORITY_*：UNSET=0, BACKGROUND=1, INTERACTIVE=2,
                    // DEFAULT=2, RECORD=3（大=高）。非法值拒绝。
                    if p > 3 {
                        return Err(crate::V4l2Error::InvalidArgument);
                    }
                    self.prio = p;
                    return Ok(());
                }
                _ => {}
            }
        }
        let mut driver = self.driver.lock();
        self.dispatcher.dispatch(&mut *driver, cmd, arg)
    }

    /// 禁用指定的 ioctl。
    pub fn disable_ioctl(&mut self, cmd: u32) {
        self.dispatcher.disable_cmd(cmd);
    }

    /// 查询 mmap 的物理地址。委托给 [`V4L2DriverOps`]（vb2 驱动的
    /// `mmap` 实现一行转发 `Vb2Queue::mmap`）。
    pub fn mmap(&self, offset: u64, length: u64) -> Option<(Vec<usize>, usize)> {
        self.driver.lock().mmap(offset, length)
    }

    /// 检查是否有数据可供 DQBUF（poll）。委托给 [`V4L2DriverOps`]。
    pub fn is_readable(&self) -> bool {
        self.driver.lock().is_readable()
    }

    /// 检查队列是否处于错误状态（poll 报 POLLERR）。委托给 [`V4L2DriverOps`]。
    pub fn is_error(&self) -> bool {
        self.driver.lock().is_error()
    }

    /// 完成事件唤醒源（DQBUF 阻塞与 VFS poll 共用）。
    /// 委托给 [`V4L2DriverOps::vb_poll_set`]。
    pub fn vb_poll_set(&self) -> Option<Arc<PollSet>> {
        self.driver.lock().vb_poll_set()
    }

    /// 事件完成唤醒源：新事件入队后唤醒（poll POLLPRI / DQEVENT 等待者）。
    /// 由 glue 在 poll register 时一并挂接（对齐 Linux `fh->wait`）。
    pub fn event_poll_set(&self) -> Arc<PollSet> {
        Arc::clone(&self.event_poll_rx)
    }

    // ── fh 生命周期 ────────────────────────────────────────────────────

    /// 设备节点被打开时调用。首次打开时创建每次 open 对应的文件句柄；
    /// 后续打开仅递增引用计数。
    pub fn open_fh(&mut self) {
        if self.fh.is_none() {
            self.fh = Some(V4l2Fh::new());
        }
        self.open_count += 1;
    }

    /// 设备节点被关闭时调用。递减引用计数；
    /// 最后一次关闭时，通过 [`V4L2DriverOps::release`] 释放流资源。
    pub fn close_fh(&mut self) {
        if self.open_count > 0 {
            self.open_count -= 1;
        }
        if self.open_count == 0 {
            self.driver.lock().release();
            self.fh = None;
        }
    }

    // ── 事件投递 ────────────────────────────────────────────────────────

    /// 向订阅了该事件类型的 fh 投递一个事件。
    ///
    /// 订阅过滤（type+id 精确匹配）、sequence 分配、每订阅队列上限与
    /// 溢出合并都在 [`V4l2Fh::queue_event`] 内完成。事件入队后唤醒
    /// `event_poll_rx`（IRQ 安全）。
    pub fn queue_event(&mut self, ev: &mut Event) {
        if let Some(fh) = &mut self.fh
            && fh.queue_event(*ev)
        {
            self.event_poll_rx.wake_from_irq(IoEvents::PRI);
        }
    }

    /// 检查当前文件句柄是否有待处理事件。
    pub fn has_pending_events(&self) -> bool {
        self.fh.as_ref().is_some_and(|fh| fh.pending() > 0)
    }

    // ── 扩展控件 ────────────────────────────────────────────────

    /// 处理 `VIDIOC_G_EXT_CTRLS`——读取当前值。
    pub fn handle_g_ext_ctrls(&self, header: &ExtControls, ctrl_array: &mut [u8]) -> Result<()> {
        let ec_size = core::mem::size_of::<ExtControl>();
        let driver = self.driver.lock();
        for i in 0..header.count as usize {
            let offset = i * ec_size;
            let ec = unsafe { &mut *(ctrl_array.as_mut_ptr().add(offset) as *mut ExtControl) };
            let mut c = Control {
                id: ec.id,
                value: 0,
            };
            driver.g_ctrl(&mut c)?;
            ec.value.value = c.value;
        }
        Ok(())
    }

    /// 处理 `VIDIOC_S_EXT_CTRLS`——设置值。
    pub fn handle_s_ext_ctrls(&mut self, _header: &ExtControls, ctrl_array: &[u8]) -> Result<()> {
        let ec_size = core::mem::size_of::<ExtControl>();
        let mut driver = self.driver.lock();
        for i in 0.._header.count as usize {
            let offset = i * ec_size;
            let ec = unsafe { &*(ctrl_array.as_ptr().add(offset) as *const ExtControl) };
            let c = Control {
                id: ec.id,
                value: unsafe { ec.value.value },
            };
            driver.s_ctrl(&c)?;
        }
        Ok(())
    }

    /// 处理 `VIDIOC_TRY_EXT_CTRLS`——只校验不应用。
    ///
    /// 没有驱动实现真正的 try 语义（无副作用地校验），
    /// 而旧的先设置再恢复的回退方案会改动
    /// volatile/有副作用的控件——因此这里返回 `NotSupported`
    /// （Linux 允许：`vidioc_try_ext_ctrls` 是可选的；错误码经
    /// `v4l2_to_axerror` 映射为 ENOTTY，与 video_ioctl2 对未实现回调
    /// 的行为一致）。不要再恢复先设置再恢复的 hack。
    pub fn handle_try_ext_ctrls(
        &mut self,
        _header: &ExtControls,
        _ctrl_array: &[u8],
    ) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }
}
