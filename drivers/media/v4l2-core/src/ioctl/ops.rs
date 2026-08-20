//! IoctlOps trait — 面向驱动的契约。
//!
//! 每个方法对应 Linux `struct v4l2_ioctl_ops` 中的一个 `VIDIOC_*` ioctl 回调。
//! 默认实现返回 `NotSupported`；
//! 驱动只需覆盖其支持的 ioctl。

use crate::{
    Result, V4l2Error,
    driver::V4L2DriverOps,
    filehandler::V4l2Fh,
    interface::{
        buffer::{Buffer, CreateBuffers, Exportbuffer, RemoveBuffers, Requestbuffers},
        capability::Capability,
        common::BufType,
        crop::{Crop, Cropcap, Selection},
        ctrl::{Control, QueryCtrl, QueryExtCtrl, Querymenu},
        event::{Event, EventSubscription},
        format::{Fmtdesc, Format, FrameIntervalEnum, FrameSizeEnum},
        inout::{Input, Output},
        stream::StreamParm,
    },
};

/// V4L2 设备驱动必须实现的 trait。
///
/// 每个方法对应一个 `vidioc_*` 回调。默认实现返回 `NotSupported`；
/// 驱动只需覆盖其支持的 ioctl。
///
/// `IoctlOps` 继承 [`V4L2DriverOps`]，因此单个驱动对象既能处理 ioctl 分发，
/// 也能处理 VFS 操作（mmap、poll、release）。
#[allow(unused_variables)]
pub trait IoctlOps: V4L2DriverOps {
    // ── 查询与枚举 ──────────────────────────────────────────

    fn querycap(&self, cap: &mut Capability) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_fmt(&self, f: &mut Fmtdesc) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_framesizes(&self, f: &mut FrameSizeEnum) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_frameintervals(&self, f: &mut FrameIntervalEnum) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 格式协商 ───────────────────────────────────────────

    fn g_fmt(&self, f: &mut Format) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_fmt(&mut self, f: &mut Format) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn try_fmt(&self, f: &mut Format) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 缓冲区管理 ───────────────────────────────────────────

    fn reqbufs(&mut self, req: &mut Requestbuffers) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn querybuf(&self, buf: &mut Buffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn qbuf(&mut self, buf: &mut Buffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn dqbuf(&mut self, buf: &mut Buffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 流式传输 ────────────────────────────────────────────────────

    fn streamon(&mut self, ty: BufType) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn streamoff(&mut self, ty: BufType) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 流参数 ─────────────────────────────────────────

    fn g_parm(&self, p: &mut StreamParm) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_parm(&mut self, p: &StreamParm) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 控制 ─────────────────────────────────────────────────────

    fn queryctrl(&self, q: &mut QueryCtrl) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_ctrl(&self, c: &mut Control) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_ctrl(&mut self, c: &Control) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn querymenu(&self, q: &mut Querymenu) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn query_ext_ctrl(&self, q: &mut QueryExtCtrl) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 输入选择 ──────────────────────────────────────────────

    fn enum_input(&self, input: &mut Input) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_input(&self) -> Result<u32> {
        Err(V4l2Error::NotSupported)
    }

    fn s_input(&mut self, index: u32) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_output(&self, output: &mut Output) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_output(&self) -> Result<u32> {
        Err(V4l2Error::NotSupported)
    }

    fn s_output(&mut self, index: u32) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 裁剪 ─────────────────────────────────────────────────────

    fn cropcap(&self, c: &mut Cropcap) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_crop(&self, c: &mut Crop) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_crop(&mut self, c: &Crop) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_selection(&self, s: &mut Selection) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_selection(&mut self, s: &Selection) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 缓冲区操作（扩展） ──────────────────────────────────

    fn prepare_buf(&mut self, buf: &mut Buffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn create_bufs(&mut self, bufs: &mut CreateBuffers) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn remove_bufs(&mut self, bufs: &mut RemoveBuffers) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn expbuf(&self, buf: &mut Exportbuffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 事件 ────────────────────────────────────────────────────────

    /// 处理 `VIDIOC_SUBSCRIBE_EVENT`。
    ///
    /// 驱动决定支持的事件类型并调用 [`V4l2Fh::subscribe`]（或
    /// [`crate::ctrls::CtrlHandler::subscribe_event`] 订阅 CTRL 事件）。
    /// 默认实现拒绝所有类型（`NotSupported`）——不支持事件的设备
    /// 的订阅必须失败，对齐 Linux 未实现 `vidioc_subscribe_event` 的
    /// 驱动（v4l2-compliance 以订阅失败判定该事件类型不存在）。
    fn subscribe_event(&mut self, _fh: &mut V4l2Fh, _sub: &EventSubscription) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// 处理 `VIDIOC_UNSUBSCRIBE_EVENT`。
    fn unsubscribe_event(&mut self, fh: &mut V4l2Fh, sub: &EventSubscription) -> Result<()> {
        fh.unsubscribe(sub);
        Ok(())
    }

    /// 处理 `VIDIOC_DQEVENT`（非阻塞）。
    ///
    /// 无待处理事件时返回 `NoEntry`（ENOENT），对齐 Linux 非阻塞
    /// `v4l2_event_dequeue`。
    fn dqevent(&mut self, fh: &mut V4l2Fh, event: &mut Event) -> Result<()> {
        *event = fh.dequeue()?;
        Ok(())
    }
}
