//! V4L2 视频设备 — 将 v4l2-core 与 StarryOS pseudofs 的 DeviceOps 桥接起来。
//!
//! 这是 `/dev/videoX` 的实现。它包装了一个 `v4l2_core::device::VideoDevice`，
//! 并处理 ioctl ABI：从用户空间指针读取 C 结构体、
//! 分发到 V4L2 ioctl 引擎，再写回结果。

use alloc::{sync::Arc, vec, vec::Vec};
use core::{any::Any, mem::MaybeUninit, task::Context};

use ax_memory_addr::PhysAddr;
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, PollSet, Pollable};
use starry_vm::{VmError, vm_read_slice, vm_write_slice};
use v4l2_core::{
    device::VideoDevice,
    error::V4l2Error,
    interface::{
        ctrl::{ExtControl, ExtControls},
        event::Event,
    },
    ioctl::IoctlCmd,
};

use crate::{
    StarryError,
    pseudofs::{DeviceMmap, DeviceOps},
};

/// 将用户内存访问错误映射为 VFS 错误（经 StarryError 桥接）。
fn vm_to_vfs(e: VmError) -> VfsError {
    VfsError::from(StarryError::from(e))
}

/// V4L2 视频设备节点 — 将 `VideoDevice` 包装为 `DeviceOps`。
pub struct V4l2DevNode {
    inner: Arc<crate::sync::Mutex<VideoDevice>>,
    /// 与驱动共享的可选事件队列。
    /// 驱动推入此队列的事件会在每次 ioctl 后被投递到文件句柄（fh）。
    event_source: Option<Arc<ax_sync::Mutex<Vec<Event>>>>,
    /// 驱动完成事件唤醒源（构造时从驱动取得）：vb2 队列内建 PollSet，
    /// `buffer_done`/`set_error` 发布状态后唤醒（IRQ 安全）——poll 等待者
    /// 挂在这里，与驱动内 DQBUF 阻塞共用（对齐 Linux vb2 done_wq 模型）。
    /// None：设备无异步完成路径，register 退化为立即唤醒。
    poll_rx: Option<Arc<PollSet>>,
    /// 事件完成唤醒源（构造时从设备取得）：新事件入队（含 SEND_INITIAL）
    /// 后唤醒 poll POLLPRI 等待者（对齐 Linux `fh->wait`）。
    event_poll_rx: Arc<PollSet>,
}

impl V4l2DevNode {
    fn new(device: VideoDevice, event_source: Option<Arc<ax_sync::Mutex<Vec<Event>>>>) -> Self {
        // 完成唤醒由驱动（vb2 队列）内建：构造时取一次 vb_poll_set，
        // 之后 register 无需设备锁。事件唤醒源同理（设备构造时内建）。
        let poll_rx = device.vb_poll_set();
        let event_poll_rx = device.event_poll_set();
        Self {
            inner: Arc::new(crate::sync::Mutex::new(device)),
            event_source,
            poll_rx,
            event_poll_rx,
        }
    }

    /// 从输入（采集）设备创建设备节点。
    ///
    /// 驱动将事件推入 `event_source`；ioctl 处理器
    /// 在每次分发后将它们排空到文件句柄中。
    pub fn from_input(device: VideoDevice, event_source: Arc<ax_sync::Mutex<Vec<Event>>>) -> Self {
        Self::new(device, Some(event_source))
    }

    /// Create a device node from an output device (no V4L2 events).
    pub fn from_output(device: VideoDevice) -> Self {
        Self::new(device, None)
    }

    /// 将共享驱动事件队列中的事件投递到 fh（订阅过滤在框架内完成）。
    fn drain_events(&self, dev: &mut VideoDevice) {
        if let Some(ref src) = self.event_source {
            let events: Vec<Event> = core::mem::take(&mut *src.lock());
            for mut ev in events {
                dev.queue_event(&mut ev);
            }
        }
    }
}

impl DeviceOps for V4l2DevNode {
    fn open(&self, _exclusive: bool) -> VfsResult<()> {
        self.inner.lock().open_fh();
        Ok(())
    }

    fn close(&self, _exclusive: bool) {
        self.inner.lock().close_fh();
    }

    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::from(StarryError::InvalidInput))
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::from(StarryError::InvalidInput))
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        // 未知命令：不解析 arg 直接 ENOTTY——对齐 Linux video_ioctl2 对未知
        // ioctl 的行为（v4l2-compliance invalid ioctl 测试传 nullptr arg，
        // 先读 arg 会 EFAULT 而非 ENOTTY）。
        let Some(ioctl_cmd) = IoctlCmd::try_from_u32(cmd) else {
            return Err(VfsError::from(StarryError::NotATty));
        };

        let mut dev = self.inner.lock();
        let size = ioctl_arg_size(cmd);
        // 堆分配 ioctl 缓冲：结构体大小可能超过栈安全阈值（如
        // v4l2_query_ext_ctrl ≈ 236B，ext_ctrls payload 可达 KB 级），
        // 栈上 1024B 数组在单核内核小栈下危险，且会截断大结构体。
        let mut buf_uninit: Vec<MaybeUninit<u8>> = vec![MaybeUninit::uninit(); size];
        if size > 0 {
            vm_read_slice(arg as *const u8, &mut buf_uninit).map_err(vm_to_vfs)?;
        }
        // MaybeUninit<u8> → u8：u8 无 drop，assume_init 安全。
        let mut buf: Vec<u8> = buf_uninit
            .into_iter()
            .map(|v| unsafe { v.assume_init() })
            .collect();

        // ── 扩展控件：需要从用户空间读取 payload ──
        match ioctl_cmd {
            IoctlCmd::GExtCtrls | IoctlCmd::SExtCtrls | IoctlCmd::TryExtCtrls => {
                // 从已复制的参数缓冲中读取头
                let header: ExtControls =
                    unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const ExtControls) };
                let ec_count = header.count as usize;
                let ec_size = core::mem::size_of::<ExtControl>();
                let payload_size = ec_count * ec_size;
                if payload_size > 0 {
                    // 从用户空间将控件数组读入堆缓冲。
                    let mut payload_uninit: Vec<MaybeUninit<u8>> =
                        vec![MaybeUninit::uninit(); payload_size];
                    vm_read_slice(header.controls as *const u8, &mut payload_uninit)
                        .map_err(vm_to_vfs)?;
                    let mut payload: Vec<u8> = payload_uninit
                        .into_iter()
                        .map(|v| unsafe { v.assume_init() })
                        .collect();

                    let result = match ioctl_cmd {
                        IoctlCmd::GExtCtrls => dev.handle_g_ext_ctrls(&header, &mut payload),
                        IoctlCmd::SExtCtrls => dev.handle_s_ext_ctrls(&header, &payload),
                        IoctlCmd::TryExtCtrls => dev.handle_try_ext_ctrls(&header, &payload),
                        _ => unreachable!(),
                    };

                    match result {
                        Ok(()) => {
                            // 为 G_EXT_CTRLS 回写控件数组
                            if ioctl_cmd == IoctlCmd::GExtCtrls {
                                vm_write_slice(header.controls as *mut u8, &payload)
                                    .map_err(vm_to_vfs)?;
                            }
                            // 同时回写头（error_idx 可能被设置）
                            let header_bytes = unsafe {
                                core::slice::from_raw_parts(
                                    &header as *const ExtControls as *const u8,
                                    ec_size,
                                )
                            };
                            vm_write_slice(arg as *mut u8, header_bytes).map_err(vm_to_vfs)?;
                            self.drain_events(&mut dev);
                            return Ok(0);
                        }
                        Err(e) => return Err(VfsError::from(v4l2_to_starry_error(e))),
                    }
                }
            }
            _ => {}
        }

        match dev.handle_ioctl(cmd, &mut buf[..size]) {
            Ok(()) => {
                if size > 0 {
                    vm_write_slice(arg as *mut u8, &buf[..size]).map_err(vm_to_vfs)?;
                }
                self.drain_events(&mut dev);
                Ok(0)
            }
            Err(e) => Err(VfsError::from(v4l2_to_starry_error(e))),
        }
    }

    fn mmap(&self, offset: u64, length: u64) -> DeviceMmap {
        let dev = self.inner.lock();
        if let Some((pages, _size)) = dev.mmap(offset, length) {
            DeviceMmap::PhysicalPages(pages.into_iter().map(PhysAddr::from_usize).collect(), None)
        } else {
            DeviceMmap::None
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

impl Pollable for V4l2DevNode {
    fn poll(&self) -> IoEvents {
        let dev = self.inner.lock();
        let mut events = IoEvents::empty();
        if dev.is_readable() {
            events |= IoEvents::IN;
        }
        // POLLERR：框架保证 ERR 位不被 events 掩掉（always_report）。
        if dev.is_error() {
            events |= IoEvents::ERR;
        }
        // POLLPRI：有待处理事件（v4l2_event_pending > 0，对齐 Linux
        // `vb2_poll` 的 `EPOLLPRI`）。select exceptfds 靠它感知 DQEVENT 可读。
        if dev.has_pending_events() {
            events |= IoEvents::PRI;
        }
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        // 注册到驱动（vb2 队列）的完成唤醒源：数据就绪（buffer_done）或
        // 队列错误（set_error）时唤醒。vivid（帧预填充）通常 poll 立即 IN，
        // 不挂起；UVC（IRQ 异步到帧）依赖此注册，否则 select/poll 挂到超时。
        // 框架在 register 后会重新 poll（io_mpx/poll.rs），无丢失唤醒。
        let Some(poll_rx) = &self.poll_rx else {
            // 设备无异步完成路径：立即唤醒让框架重查电平。
            context.waker().wake_by_ref();
            return;
        };
        let interests = events & (IoEvents::IN | IoEvents::ERR);
        if !interests.is_empty() {
            // SAFETY: register 从任务上下文（poll 路径）调用，且不持设备锁，
            // 满足 PollSet 约束。
            unsafe { poll_rx.register(context.waker(), interests) };
        }
        // 事件唤醒源：新事件入队（含 SEND_INITIAL）后唤醒 POLLPRI 等待者。
        if !(events & IoEvents::PRI).is_empty() {
            // SAFETY: 同上——任务上下文调用、不持设备锁。
            unsafe {
                self.event_poll_rx
                    .register(context.waker(), IoEvents::PRI);
            }
        }
    }
}

/// 估算给定 ioctl 命令对应的 C 结构体大小。
fn ioctl_arg_size(cmd: u32) -> usize {
    let encoded = ((cmd >> 16) & 0x3FFF) as usize;
    if encoded == 0 { 256 } else { encoded }
}

/// 将 V4L2 错误映射为 StarryOS 错误。
///
/// 唯一的 V4l2Error → 内核错误映射点（v4l2-core 内部不再维护 errno 表）。
/// StarryError → Linux errno 的最终转换由内核错误基础设施完成。
fn v4l2_to_starry_error(e: V4l2Error) -> StarryError {
    match e {
        V4l2Error::InvalidArgument => StarryError::InvalidInput,
        V4l2Error::NoSuchDevice => StarryError::NoSuchDevice,
        V4l2Error::Io => StarryError::Io,
        V4l2Error::NotSupported => StarryError::NotATty,
        V4l2Error::Busy => StarryError::ResourceBusy,
        V4l2Error::Timeout => StarryError::TimedOut,
        V4l2Error::NoMemory => StarryError::NoMemory,
        V4l2Error::AccessDenied => StarryError::PermissionDenied,
        V4l2Error::BadFileDescriptor => StarryError::BadFileDescriptor,
        V4l2Error::WouldBlock => StarryError::WouldBlock,
        V4l2Error::NoEntry => StarryError::NotFound,
        V4l2Error::NoSuchDeviceOrAddress => StarryError::NoSuchDeviceOrAddress,
        V4l2Error::OperationNotPermitted => StarryError::OperationNotPermitted,
        V4l2Error::Interrupted => StarryError::Interrupted,
        V4l2Error::NotATty => StarryError::NotATty,
        V4l2Error::StorageFull => StarryError::StorageFull,
        V4l2Error::OutOfRange => StarryError::OutOfRange,
        V4l2Error::MessageTooLong => StarryError::Io,
    }
}
