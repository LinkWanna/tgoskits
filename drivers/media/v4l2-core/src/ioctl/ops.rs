//! IoctlOps trait — 面向驱动的契约。
//!
//! 每个方法对应 Linux `struct v4l2_ioctl_ops` 中的一个 `VIDIOC_*` ioctl 回调。
//! 默认实现返回 `NotSupported`；驱动只需覆盖其支持的 ioctl。
//!
//! V4L2 的全部 83 个 ioctl 按历史划分为两个 trait：
//! - [`IoctlOps`]：modern 接口（49 个），现代驱动仍实现；
//! - [`LegacyIoctlOps`]：legacy 接口（34 个），实质废弃、新设备不再实现
//!   或不再需要驱动实现（默认全部返回 `NotSupported`）。
//!
//! 二者由 [`crate::driver::V4L2DriverOps`] 聚合：`V4L2DriverOps` 是
//! [`IoctlOps`] 与 [`LegacyIoctlOps`] 的 supertrait，供
//! [`crate::device::VideoDevice`] 以单个 trait 对象持有驱动。

use crate::{
    Result, V4l2Error,
    filehandler::V4l2Fh,
    interface::{
        BufType,
        buffer::{Buffer, CreateBuffers, Exportbuffer, RemoveBuffers, Requestbuffers},
        capability::Capability,
        crop::{Crop, Cropcap, Selection},
        ctrl::Control,
        dv::{DvTimings, DvTimingsCap, EnumDvTimings},
        edid::Edid,
        event::{Event, EventSubscription},
        format::{Fmtdesc, Format, FrameIntervalEnum, FrameSizeEnum},
        inout::{Input, Output, StdId},
        legacy::{
            audio::{Audio, AudioOut},
            codec::{DecoderCmd, EncIndex, EncoderCmd},
            debug::{DbgChipInfo, DbgRegister},
            framebuffer::Framebuffer,
            jpegcomp::JpegCompression,
            modulator::Modulator,
            standard::Standard,
            tuner::{Frequency, FrequencyBand, HwFreqSeek, Tuner},
            vbi::SlicedVbiCap,
        },
        stream::StreamParm,
    },
};

/// V4L2 视频设备驱动必须实现的 trait（modern ioctl 集）。
///
/// 每个方法对应一个 `vidioc_*` 回调。其中查询能力、格式协商、
/// 缓冲管理与流式传输的方法是任何 V4L2 流媒体设备的核心路径，
/// **必须**由驱动实现（无默认实现）；其余按设备能力可选实现，
/// 默认返回 `NotSupported`。
///
/// 由 [`crate::driver::V4L2DriverOps`] 聚合（作为其 supertrait），
/// 因此单个驱动对象既能处理 ioctl 分发，也能处理 VFS 操作
/// （mmap、poll、release）。
#[allow(unused_variables)]
pub trait IoctlOps {
    // ── 查询与枚举 ──────────────────────────────────────────

    /// 查询设备能力（`VIDIOC_QUERYCAP`）
    fn querycap(&self, cap: &mut Capability) -> Result<()>;

    /// 枚举像素格式（`VIDIOC_ENUM_FMT`）
    fn enum_fmt(&self, f: &mut Fmtdesc) -> Result<()>;

    /// 枚举帧尺寸（`VIDIOC_ENUM_FRAMESIZES`）
    fn enum_framesizes(&self, f: &mut FrameSizeEnum) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// 枚举帧间隔（`VIDIOC_ENUM_FRAMEINTERVALS`）
    fn enum_frameintervals(&self, f: &mut FrameIntervalEnum) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 格式协商 ───────────────────────────────────────────

    /// 获取当前格式（`VIDIOC_G_FMT`）
    fn g_fmt(&self, f: &mut Format) -> Result<()>;

    /// 设置格式（`VIDIOC_S_FMT`）
    fn s_fmt(&mut self, f: &mut Format) -> Result<()>;

    /// 试设格式（`VIDIOC_TRY_FMT`）
    fn try_fmt(&self, f: &mut Format) -> Result<()>;

    // ── 缓冲区管理 ───────────────────────────────────────────

    /// 申请缓冲区（`VIDIOC_REQBUFS`）
    fn reqbufs(&mut self, req: &mut Requestbuffers) -> Result<()>;

    /// 查询缓冲区（`VIDIOC_QUERYBUF`）
    fn querybuf(&self, buf: &mut Buffer) -> Result<()>;

    /// 入队缓冲区（`VIDIOC_QBUF`）
    fn qbuf(&mut self, buf: &mut Buffer) -> Result<()>;

    /// 出队缓冲区（`VIDIOC_DQBUF`）
    fn dqbuf(&mut self, buf: &mut Buffer) -> Result<()>;

    /// 预备缓冲区（`VIDIOC_PREPARE_BUF`）
    fn prepare_buf(&mut self, buf: &mut Buffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// 创建缓冲区（`VIDIOC_CREATE_BUFS`）
    fn create_bufs(&mut self, bufs: &mut CreateBuffers) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// 移除缓冲区（`VIDIOC_REMOVE_BUFS`）
    fn remove_bufs(&mut self, bufs: &mut RemoveBuffers) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// 导出缓冲区（`VIDIOC_EXPBUF`）。仅支持 DMABUF 时实现
    fn expbuf(&self, buf: &mut Exportbuffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 流式传输 ────────────────────────────────────────────────────

    /// 开启流（`VIDIOC_STREAMON`）
    fn streamon(&mut self, ty: BufType) -> Result<()>;

    /// 关闭流（`VIDIOC_STREAMOFF`）
    fn streamoff(&mut self, ty: BufType) -> Result<()>;

    // ── 流参数 ─────────────────────────────────────────

    fn g_parm(&self, p: &mut StreamParm) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_parm(&mut self, p: &StreamParm) -> Result<()> {
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

    // ── 裁剪 / Selection ─────────────────────────────────────────

    fn cropcap(&self, c: &mut Cropcap) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_selection(&self, s: &mut Selection) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_selection(&mut self, s: &Selection) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── EDID ─────────────────────────────────────────────────────

    fn g_edid(&self, edid: &mut Edid) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_edid(&mut self, edid: &mut Edid) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── DV timings ───────────────────────────────────────────────

    fn g_dv_timings(&self, t: &mut DvTimings) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_dv_timings(&mut self, t: &mut DvTimings) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_dv_timings(&self, t: &mut EnumDvTimings) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn query_dv_timings(&self, t: &mut DvTimings) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn dv_timings_cap(&self, c: &mut DvTimingsCap) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 日志 ─────────────────────────────────────────────────────

    fn log_status(&self) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 事件 ────────────────────────────────────────────────────────

    /// 处理 `VIDIOC_SUBSCRIBE_EVENT`。
    fn subscribe_event(&mut self, _fh: &mut V4l2Fh, _sub: &EventSubscription) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// 处理 `VIDIOC_UNSUBSCRIBE_EVENT`。
    fn unsubscribe_event(&mut self, fh: &mut V4l2Fh, sub: &EventSubscription) -> Result<()> {
        fh.unsubscribe(sub);
        Ok(())
    }

    /// 处理 `VIDIOC_DQEVENT`（非阻塞）。
    fn dqevent(&mut self, fh: &mut V4l2Fh, event: &mut Event) -> Result<()> {
        *event = fh.dequeue()?;
        Ok(())
    }
}

/// 遗留 V4L2 ioctl 驱动的 trait（36 个，默认全部返回 `NotSupported`）。
#[allow(unused_variables)]
pub trait LegacyIoctlOps {
    // ── 弃用控件（G_CTRL / S_CTRL）──────────────────────────────

    /// 处理 `VIDIOC_G_CTRL`（弃用）。
    ///
    /// 驱动不再实现该方法；核心层经 [`crate::driver::V4L2DriverOps::ctrl_handler`]
    /// 统一处理。此处默认返回 `NotSupported`，仅作为遗留分发兜底。
    fn g_ctrl(&self, _c: &mut Control) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// 处理 `VIDIOC_S_CTRL`（弃用）。
    ///
    /// 驱动不再实现该方法；核心层经 [`crate::driver::V4L2DriverOps::ctrl_handler`]
    /// 统一处理。此处默认返回 `NotSupported`，仅作为遗留分发兜底。
    fn s_ctrl(&mut self, _c: &Control) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── Overlay 帧缓冲 ─────────────────────────────────────────

    fn g_fbuf(&self, fb: &mut Framebuffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_fbuf(&mut self, fb: &Framebuffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn overlay(&mut self, on: u32) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 模拟电视标准 ───────────────────────────────────────────

    fn g_std(&self, id: &mut StdId) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_std(&mut self, id: StdId) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_std(&self, s: &mut Standard) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn query_std(&self, id: &mut StdId) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── Tuner/Radio ────────────────────────────────────────────

    fn g_tuner(&self, t: &mut Tuner) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_tuner(&mut self, t: &Tuner) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_frequency(&self, f: &mut Frequency) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_frequency(&mut self, f: &Frequency) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_freq_bands(&self, b: &mut FrequencyBand) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_hw_freq_seek(&mut self, s: &HwFreqSeek) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 调制器 ─────────────────────────────────────────────────

    fn g_modulator(&self, m: &mut Modulator) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_modulator(&mut self, m: &Modulator) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 音频 I/O ───────────────────────────────────────────────

    fn g_audio(&self, a: &mut Audio) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_audio(&mut self, a: &Audio) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_audio(&self, a: &mut Audio) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_audout(&self, a: &mut AudioOut) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_audout(&mut self, a: &AudioOut) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_audout(&self, a: &mut AudioOut) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 裁剪旧 API ─────────────────────────────────────────────

    fn g_crop(&self, c: &mut Crop) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_crop(&mut self, c: &Crop) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── JPEG 压缩旧 API ────────────────────────────────────────

    fn g_jpegcomp(&self, j: &mut JpegCompression) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_jpegcomp(&mut self, j: &JpegCompression) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── Sliced VBI ─────────────────────────────────────────────

    fn g_sliced_vbi_cap(&self, c: &mut SlicedVbiCap) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── Stateful codec ─────────────────────────────────────────

    fn g_enc_index(&self, idx: &mut EncIndex) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn encoder_cmd(&mut self, c: &mut EncoderCmd) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn try_encoder_cmd(&mut self, c: &mut EncoderCmd) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn decoder_cmd(&mut self, c: &mut DecoderCmd) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn try_decoder_cmd(&mut self, c: &mut DecoderCmd) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 调试 ───────────────────────────────────────────────────

    fn dbg_g_register(&self, r: &mut DbgRegister) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn dbg_s_register(&mut self, r: &DbgRegister) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn dbg_g_chip_info(&self, c: &mut DbgChipInfo) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }
}
