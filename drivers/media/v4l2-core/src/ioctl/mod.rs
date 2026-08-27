//! IoctlCmd — V4L2 ioctl 命令枚举及分发。
//!
//! 相当于 Linux 的 `__video_do_ioctl()` + `v4l2_ioctls[]` 分发表。
//! 每个 ioctl 命令是一个 `#[repr(u32)]` 枚举变体，其判别值为与 Linux
//! 兼容的命令号。`dispatch` 方法通过 match 直接映射到
//! [`IoctlOps`]（modern）或 [`LegacyIoctlOps`]（遗留）回调。
//!
//! 全部 83 个 VIDIOC 命令按 Linux 历史划分为：
//! - **modern**：现代驱动仍实现的活动接口（47 个，[`IoctlCmd`]）
//! - **legacy**：实质废弃、新设备不再实现或不再需要驱动实现的接口
//!   （36 个，[`LegacyIoctlCmd`]，含弃用的 G/S_CTRL）

mod ops;

use core::{mem, ptr};

pub use ops::{IoctlOps, LegacyIoctlOps};

use crate::{
    V4l2Error,
    driver::V4L2DriverOps,
    interface::{
        BufType,
        buffer::{Buffer, CreateBuffers, Exportbuffer, RemoveBuffers, Requestbuffers},
        capability::Capability,
        crop::{Crop, Cropcap, Selection},
        ctrl::{Control, ExtControls, QueryCtrl, QueryExtCtrl, Querymenu},
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

type Result<T> = core::result::Result<T, V4l2Error>;

// ── IOCTL 代码生成辅助函数 ───────────────────────────────────────

const DIR_WRITE: u32 = 1;
const DIR_READ: u32 = 2;
const DIRSHIFT: u32 = 30;
const VT: u8 = b'V';

const fn io(nr: u8) -> u32 {
    ((VT as u32) << 8) | (nr as u32)
}
const fn ior(nr: u8, size: u32) -> u32 {
    (DIR_READ << DIRSHIFT) | ((VT as u32) << 8) | (nr as u32) | (size << 16)
}
const fn iow(nr: u8, size: u32) -> u32 {
    (DIR_WRITE << DIRSHIFT) | ((VT as u32) << 8) | (nr as u32) | (size << 16)
}
const fn iowr(nr: u8, size: u32) -> u32 {
    ((DIR_READ | DIR_WRITE) << DIRSHIFT) | ((VT as u32) << 8) | (nr as u32) | (size << 16)
}

// ── 读/写辅助函数 ──────────────────────────────────────────────────

/// 从原始字节中读取一个值（不安全：调用方需保证类型与大小正确）。
pub(crate) unsafe fn read_from_bytes<T: Copy>(bytes: &[u8]) -> T {
    assert!(bytes.len() >= mem::size_of::<T>());
    let ptr = bytes.as_ptr() as *const T;
    unsafe { ptr::read_unaligned(ptr) }
}

/// 将值写入原始字节。
pub(crate) unsafe fn write_to_bytes<T: Copy>(bytes: &mut [u8], val: &T) {
    assert!(bytes.len() >= mem::size_of::<T>());
    let ptr = bytes.as_mut_ptr() as *mut T;
    unsafe { ptr::write_unaligned(ptr, *val) };
}

// ── 命令枚举 ────────────────────────────────────────────────────────

macro_rules! ioctl_defs {
    ($name:ident, $(($variant:ident, $dir:ident, $nr:expr, $ty:ty)),* $(,)?) => {
        /// V4L2 ioctl 命令 — 每个变体对应一个 `VIDIOC_*` 命令。
        ///
        /// 判别值与 Linux ABI 完全一致。
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u32)]
        pub enum $name {
            $($variant = ioctl_defs!(@val $dir, $nr, $ty),)*
        }

        impl $name {
            pub const COUNT: usize = [$(Self::$variant),*].len();
            pub const ALL: [$name; $name::COUNT] = [$(Self::$variant),*];

            pub fn try_from_u32(cmd: u32) -> Option<Self> {
                Some(match cmd {
                    $(c if c == Self::$variant as u32 => Self::$variant,)*
                    _ => return None,
                })
            }
        }
    };
    (@val io, $nr:expr, $ty:ty) => { io($nr) };
    (@val ior, $nr:expr, $ty:ty) => { ior($nr, core::mem::size_of::<$ty>() as u32) };
    (@val iow, $nr:expr, $ty:ty) => { iow($nr, core::mem::size_of::<$ty>() as u32) };
    (@val iowr, $nr:expr, $ty:ty) => { iowr($nr, core::mem::size_of::<$ty>() as u32) };
}

// 现代 V4L2 ioctl 命令（47 个）。
ioctl_defs!(
    IoctlCmd,
    // ── 查询与枚举 ──────────────────────────────────────────
    (QueryCap, ior, 0, Capability),
    (EnumFmt, iowr, 2, Fmtdesc),
    (EnumFrameSizes, iowr, 74, FrameSizeEnum),
    (EnumFrameIntervals, iowr, 75, FrameIntervalEnum),
    // ── 格式协商 ───────────────────────────────────────────
    (GFmt, iowr, 4, Format),
    (SFmt, iowr, 5, Format),
    (TryFmt, iowr, 64, Format),
    // ── 缓冲区管理 ────────────────────────────────────────────
    (ReqBufs, iowr, 8, Requestbuffers),
    (QueryBuf, iowr, 9, Buffer),
    (QBuf, iowr, 15, Buffer),
    (DQBuf, iowr, 17, Buffer),
    (PrepareBuf, iowr, 93, Buffer),
    (CreateBufs, iowr, 92, CreateBuffers),
    (RemoveBufs, iowr, 104, RemoveBuffers),
    (ExpBuf, iowr, 16, Exportbuffer),
    // ── 流式传输 ────────────────────────────────────────────────────
    (StreamOn, iow, 18, i32),
    (StreamOff, iow, 19, i32),
    // ── 流参数 ─────────────────────────────────────────
    (GParm, iowr, 21, StreamParm),
    (SParm, iowr, 22, StreamParm),
    // ── 优先级（core 层维护，device.rs 拦截） ──────────────
    (GPriority, ior, 67, u32),
    (SPriority, iow, 68, u32),
    // ── 输入/输出选择 ─────────────────────────────────────
    (EnumInput, iowr, 26, Input),
    (GInput, ior, 38, i32),
    (SInput, iowr, 39, i32),
    (EnumOutput, iowr, 48, Output),
    (GOutput, ior, 46, i32),
    (SOutput, iowr, 47, i32),
    // ── 控制 ─────────────────────────────────────────────────────
    (QueryCtrl, iowr, 36, QueryCtrl),
    (QueryExtCtrl, iowr, 103, QueryExtCtrl),
    (GExtCtrls, iowr, 71, ExtControls),
    (SExtCtrls, iowr, 72, ExtControls),
    (TryExtCtrls, iowr, 73, ExtControls),
    (QueryMenu, iowr, 37, Querymenu),
    // ── 裁剪 / Selection ─────────────────────────────────────────
    (CropCap, iowr, 58, Cropcap),
    (GSelection, iowr, 94, Selection),
    (SSelection, iowr, 95, Selection),
    // ── EDID ─────────────────────────────────────────────────────
    (GEdid, iowr, 40, Edid),
    (SEdid, iowr, 41, Edid),
    // ── DV timings ───────────────────────────────────────────────
    (SDvTimings, iowr, 87, DvTimings),
    (GDvTimings, iowr, 88, DvTimings),
    (EnumDvTimings, iowr, 98, EnumDvTimings),
    (QueryDvTimings, ior, 99, DvTimings),
    (DvTimingsCap, iowr, 100, DvTimingsCap),
    // ── 日志 ─────────────────────────────────────────────────────
    (LogStatus, io, 70, ()),
    // ── 事件（device.rs 拦截路由到驱动回调） ──────────────────
    (DQEvent, ior, 89, Event),
    (SubscribeEvent, iow, 90, EventSubscription),
    (UnsubscribeEvent, iow, 91, EventSubscription),
);

// 遗留 V4L2 ioctl 命令（36 个）
ioctl_defs!(
    LegacyIoctlCmd,
    // ── G/S_CTRL ───────
    (GCtrl, iowr, 27, Control),
    (SCtrl, iowr, 28, Control),
    // ── Overlay 帧缓冲 ──────────────────────────────────────
    (GFbuf, ior, 10, Framebuffer),
    (SFbuf, iow, 11, Framebuffer),
    (Overlay, iow, 14, i32),
    // ── 模拟电视标准 ───────────────────────────────────
    (GStd, ior, 23, StdId),
    (SStd, iow, 24, StdId),
    (EnumStd, iowr, 25, Standard),
    (QueryStd, ior, 63, StdId),
    // ── Tuner/Radio ────────────────────────────────────
    (GTuner, iowr, 29, Tuner),
    (STuner, iow, 30, Tuner),
    (GFrequency, iowr, 56, Frequency),
    (SFrequency, iow, 57, Frequency),
    (EnumFreqBands, iowr, 101, FrequencyBand),
    (SHwFreqSeek, iow, 82, HwFreqSeek),
    // ── 调制器 ─────────────────────────────────────────
    (GModulator, iowr, 54, Modulator),
    (SModulator, iow, 55, Modulator),
    // ── 音频 I/O ───────────────────────────────────────
    (GAudio, ior, 33, Audio),
    (SAudio, iow, 34, Audio),
    (EnumAudio, iowr, 65, Audio),
    (GAudioOut, ior, 49, AudioOut),
    (SAudioOut, iow, 50, AudioOut),
    (EnumAudioOut, iowr, 66, AudioOut),
    // ── 裁剪旧 API ─────────────────────────────────────
    (GCrop, iowr, 59, Crop),
    (SCrop, iow, 60, Crop),
    // ── JPEG 压缩旧 API ────────────────────────────────
    (GJpegComp, ior, 61, JpegCompression),
    (SJpegComp, iow, 62, JpegCompression),
    // ── Sliced VBI ─────────────────────────────────────
    (GSlicedVbiCap, iowr, 69, SlicedVbiCap),
    // ── Stateful codec ─────────────────────────────────
    (GEncIndex, ior, 76, EncIndex),
    (EncoderCmd, iowr, 77, EncoderCmd),
    (TryEncoderCmd, iowr, 78, EncoderCmd),
    (DecoderCmd, iowr, 96, DecoderCmd),
    (TryDecoderCmd, iowr, 97, DecoderCmd),
    // ── 调试 ───────────────────────────────────────────
    (DbgSRegister, iow, 79, DbgRegister),
    (DbgGRegister, iowr, 80, DbgRegister),
    (DbgGChipInfo, iowr, 102, DbgChipInfo),
);

/// 表驱动生成 `dispatch` 的 match 表达式。
///
/// `self` 是待分发的 ioctl 命令（[`IoctlCmd`] 或 [`LegacyIoctlCmd`]），
/// `ops` 是完整驱动对象（[`V4L2DriverOps`]），`arg` 是用户态字节切片。
///
/// 单命令条目按模式展开为一条 match 分支（`=>` 后为表达式位置的
/// 辅助宏调用）：
/// - `rw(variant, method, ty)`：读 `ty` → `ops.method(&mut v)?` → 写回（IOWR/IOR）
/// - `wo(variant, method, ty)`：读 `ty` → `ops.method(&v)`（IOW，不回写）
/// - `get(variant, method)`：`ops.method()?` 返回标量 → 写回
/// - `val(variant, method, ty)`：读标量 `ty` → `ops.method(v)`（按值传递）
/// - `buf_type(variant, method)`：读 `u32` → [`BufType`] → `ops.method(bt)`
/// - `noarg(variant, method)`：`ops.method()`（无参 ioctl）
///
/// `;` 之后是 `none(variant, ...)` 组：一组命令统一返回 `NotSupported`
/// （core 层代管的 ioctl，如 priority/event/ext_ctrls）。
/// 生成单条 match 分支体（`=>` 后的表达式）的辅助宏。
///
/// 模式：
/// - `rw`：读 `ty` → `ops.method(&mut v)?` → 写回（IOWR/IOR）
/// - `rw_ctrl`：读 `ty` → 经 `ops.ctrl_handler()` 处理 → 写回（控件 ioctl）
/// - `wo`：读 `ty` → `ops.method(&v)`（IOW，不回写）
/// - `get`：`ops.method()?` 返回标量 → 写回
/// - `val`：读标量 `ty` → `ops.method(v)`（按值传递）
/// - `buf_type`：读 `u32` → [`BufType`] → `ops.method(bt)`
/// - `noarg`：`ops.method()`（无参 ioctl）
macro_rules! ioctl_body {
    (rw, $ops:ident, $arg:ident, $method:ident, $ty:ty) => {{
        let mut v: $ty = read_from_bytes($arg);
        $ops.$method(&mut v)?;
        write_to_bytes($arg, &v);
        Ok(())
    }};
    (rw_ctrl, $ops:ident, $arg:ident, $method:ident, $ty:ty) => {{
        let mut v: $ty = read_from_bytes($arg);
        let handler = $ops.ctrl_handler().ok_or(V4l2Error::NotSupported)?;
        handler.$method(&mut v)?;
        write_to_bytes($arg, &v);
        Ok(())
    }};
    (wo, $ops:ident, $arg:ident, $method:ident, $ty:ty) => {{
        let v: $ty = read_from_bytes($arg);
        $ops.$method(&v)
    }};
    (get, $ops:ident, $arg:ident, $method:ident) => {{
        let v = $ops.$method()?;
        write_to_bytes($arg, &v);
        Ok(())
    }};
    (val, $ops:ident, $arg:ident, $method:ident, $ty:ty) => {{
        let v: $ty = read_from_bytes($arg);
        $ops.$method(v)
    }};
    (buf_type, $ops:ident, $arg:ident, $method:ident) => {{
        let ty: u32 = read_from_bytes($arg);
        let bt = BufType::try_from_u32(ty).ok_or(V4l2Error::InvalidArgument)?;
        $ops.$method(bt)
    }};
    (noarg, $ops:ident, $arg:ident, $method:ident) => {{ $ops.$method() }};
}

impl IoctlCmd {
    #[allow(clippy::too_many_lines)]
    pub fn dispatch(self, ops: &mut dyn V4L2DriverOps, arg: &mut [u8]) -> Result<()> {
        // SAFETY: `arg` 长度已由 VFS 层按 ioctl 编码保证，
        // `read_from_bytes`/`write_to_bytes` 仅在长度足够时访问。
        unsafe {
            match self {
                // ── 查询与枚举 ──────────────────────────────
                Self::QueryCap => ioctl_body!(rw, ops, arg, querycap, Capability),
                Self::EnumFmt => ioctl_body!(rw, ops, arg, enum_fmt, Fmtdesc),
                Self::EnumFrameSizes => {
                    ioctl_body!(rw, ops, arg, enum_framesizes, FrameSizeEnum)
                }
                Self::EnumFrameIntervals => {
                    ioctl_body!(rw, ops, arg, enum_frameintervals, FrameIntervalEnum)
                }

                // ── 格式协商 ───────────────────────────────
                Self::GFmt => ioctl_body!(rw, ops, arg, g_fmt, Format),
                Self::SFmt => ioctl_body!(rw, ops, arg, s_fmt, Format),
                Self::TryFmt => ioctl_body!(rw, ops, arg, try_fmt, Format),

                // ── 缓冲区管理 ────────────────────────────────
                Self::ReqBufs => ioctl_body!(rw, ops, arg, reqbufs, Requestbuffers),
                Self::QueryBuf => ioctl_body!(rw, ops, arg, querybuf, Buffer),
                Self::QBuf => ioctl_body!(rw, ops, arg, qbuf, Buffer),
                Self::DQBuf => ioctl_body!(rw, ops, arg, dqbuf, Buffer),
                Self::PrepareBuf => ioctl_body!(rw, ops, arg, prepare_buf, Buffer),
                Self::CreateBufs => ioctl_body!(rw, ops, arg, create_bufs, CreateBuffers),
                Self::RemoveBufs => ioctl_body!(rw, ops, arg, remove_bufs, RemoveBuffers),
                Self::ExpBuf => ioctl_body!(rw, ops, arg, expbuf, Exportbuffer),

                // ── 流式传输 ────────────────────────────────────
                Self::StreamOn => ioctl_body!(buf_type, ops, arg, streamon),
                Self::StreamOff => ioctl_body!(buf_type, ops, arg, streamoff),

                // ── 流参数 ─────────────────────────────
                Self::GParm => ioctl_body!(rw, ops, arg, g_parm, StreamParm),
                Self::SParm => ioctl_body!(wo, ops, arg, s_parm, StreamParm),

                // ── 输入/输出选择 ─────────────────────────
                Self::EnumInput => ioctl_body!(rw, ops, arg, enum_input, Input),
                Self::GInput => ioctl_body!(get, ops, arg, g_input),
                Self::SInput => ioctl_body!(val, ops, arg, s_input, u32),
                Self::EnumOutput => ioctl_body!(rw, ops, arg, enum_output, Output),
                Self::GOutput => ioctl_body!(get, ops, arg, g_output),
                Self::SOutput => ioctl_body!(val, ops, arg, s_output, u32),

                // ── 控件查询（经驱动 CtrlHandler，核心统一处理）────
                Self::QueryCtrl => ioctl_body!(rw_ctrl, ops, arg, queryctrl, QueryCtrl),
                Self::QueryExtCtrl => {
                    ioctl_body!(rw_ctrl, ops, arg, query_ext_ctrl, QueryExtCtrl)
                }
                Self::QueryMenu => ioctl_body!(rw_ctrl, ops, arg, querymenu, Querymenu),

                // ── 核心代管 ioctl（priority / ext_ctrls / event）──
                // 由 VideoDevice::handle_ioctl 或 pseudofs glue 拦截路由，
                // 不进此分发器。
                Self::GPriority
                | Self::SPriority
                | Self::GExtCtrls
                | Self::SExtCtrls
                | Self::TryExtCtrls
                | Self::DQEvent
                | Self::SubscribeEvent
                | Self::UnsubscribeEvent => Err(V4l2Error::NotSupported),

                // ── 裁剪 / Selection ─────────────────────────────
                Self::CropCap => ioctl_body!(rw, ops, arg, cropcap, Cropcap),
                Self::GSelection => ioctl_body!(rw, ops, arg, g_selection, Selection),
                Self::SSelection => ioctl_body!(wo, ops, arg, s_selection, Selection),

                // ── EDID ─────────────────────────────────────────
                Self::GEdid => ioctl_body!(rw, ops, arg, g_edid, Edid),
                Self::SEdid => ioctl_body!(rw, ops, arg, s_edid, Edid),

                // ── DV timings ───────────────────────────────────
                Self::GDvTimings => ioctl_body!(rw, ops, arg, g_dv_timings, DvTimings),
                Self::SDvTimings => ioctl_body!(rw, ops, arg, s_dv_timings, DvTimings),
                Self::EnumDvTimings => {
                    ioctl_body!(rw, ops, arg, enum_dv_timings, EnumDvTimings)
                }
                Self::QueryDvTimings => ioctl_body!(rw, ops, arg, query_dv_timings, DvTimings),
                Self::DvTimingsCap => ioctl_body!(rw, ops, arg, dv_timings_cap, DvTimingsCap),

                // ── 日志 ────────────────────────────────────────
                Self::LogStatus => ioctl_body!(noarg, ops, arg, log_status),
            }
        }
    }
}

impl LegacyIoctlCmd {
    #[allow(clippy::too_many_lines)]
    pub fn dispatch(self, ops: &mut dyn V4L2DriverOps, arg: &mut [u8]) -> Result<()> {
        // SAFETY: `arg` 长度已由 VFS 层按 ioctl 编码保证。
        unsafe {
            match self {
                Self::GCtrl => ioctl_body!(rw_ctrl, ops, arg, g_ctrl, Control),
                Self::SCtrl => ioctl_body!(rw_ctrl, ops, arg, s_ctrl, Control),

                // ── Overlay 帧缓冲 ────────────────────────────
                Self::GFbuf => ioctl_body!(rw, ops, arg, g_fbuf, Framebuffer),
                Self::SFbuf => ioctl_body!(wo, ops, arg, s_fbuf, Framebuffer),
                Self::Overlay => ioctl_body!(val, ops, arg, overlay, u32),

                // ── 模拟电视标准 ──────────────────────────────
                Self::GStd => ioctl_body!(rw, ops, arg, g_std, StdId),
                Self::SStd => ioctl_body!(val, ops, arg, s_std, StdId),
                Self::EnumStd => ioctl_body!(rw, ops, arg, enum_std, Standard),
                Self::QueryStd => ioctl_body!(rw, ops, arg, query_std, StdId),

                // ── Tuner/Radio ───────────────────────────────
                Self::GTuner => ioctl_body!(rw, ops, arg, g_tuner, Tuner),
                Self::STuner => ioctl_body!(wo, ops, arg, s_tuner, Tuner),
                Self::GFrequency => ioctl_body!(rw, ops, arg, g_frequency, Frequency),
                Self::SFrequency => ioctl_body!(wo, ops, arg, s_frequency, Frequency),
                Self::EnumFreqBands => {
                    ioctl_body!(rw, ops, arg, enum_freq_bands, FrequencyBand)
                }
                Self::SHwFreqSeek => ioctl_body!(wo, ops, arg, s_hw_freq_seek, HwFreqSeek),

                // ── 调制器 ─────────────────────────────────────
                Self::GModulator => ioctl_body!(rw, ops, arg, g_modulator, Modulator),
                Self::SModulator => ioctl_body!(wo, ops, arg, s_modulator, Modulator),

                // ── 音频 I/O ───────────────────────────────────
                Self::GAudio => ioctl_body!(rw, ops, arg, g_audio, Audio),
                Self::SAudio => ioctl_body!(wo, ops, arg, s_audio, Audio),
                Self::EnumAudio => ioctl_body!(rw, ops, arg, enum_audio, Audio),
                Self::GAudioOut => ioctl_body!(rw, ops, arg, g_audout, AudioOut),
                Self::SAudioOut => ioctl_body!(wo, ops, arg, s_audout, AudioOut),
                Self::EnumAudioOut => ioctl_body!(rw, ops, arg, enum_audout, AudioOut),

                // ── 裁剪旧 API ────────────────────────────────
                Self::GCrop => ioctl_body!(rw, ops, arg, g_crop, Crop),
                Self::SCrop => ioctl_body!(wo, ops, arg, s_crop, Crop),

                // ── JPEG 压缩旧 API ───────────────────────────
                Self::GJpegComp => ioctl_body!(rw, ops, arg, g_jpegcomp, JpegCompression),
                Self::SJpegComp => ioctl_body!(wo, ops, arg, s_jpegcomp, JpegCompression),

                // ── Sliced VBI ─────────────────────────────────
                Self::GSlicedVbiCap => {
                    ioctl_body!(rw, ops, arg, g_sliced_vbi_cap, SlicedVbiCap)
                }

                // ── Stateful codec ─────────────────────────────
                Self::GEncIndex => ioctl_body!(rw, ops, arg, g_enc_index, EncIndex),
                Self::EncoderCmd => ioctl_body!(rw, ops, arg, encoder_cmd, EncoderCmd),
                Self::TryEncoderCmd => ioctl_body!(rw, ops, arg, try_encoder_cmd, EncoderCmd),
                Self::DecoderCmd => ioctl_body!(rw, ops, arg, decoder_cmd, DecoderCmd),
                Self::TryDecoderCmd => ioctl_body!(rw, ops, arg, try_decoder_cmd, DecoderCmd),

                // ── 调试 ───────────────────────────────────────
                Self::DbgGRegister => ioctl_body!(rw, ops, arg, dbg_g_register, DbgRegister),
                Self::DbgSRegister => ioctl_body!(wo, ops, arg, dbg_s_register, DbgRegister),
                Self::DbgGChipInfo => ioctl_body!(rw, ops, arg, dbg_g_chip_info, DbgChipInfo),
            }
        }
    }
}

// ── 统一命令枚举（modern + legacy）──────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoIoctl {
    Modern(IoctlCmd),
    Legacy(LegacyIoctlCmd),
}

impl VideoIoctl {
    /// 由原始 ioctl 命令号归一化；未知命令返回 `None`（对应 ENOTTY）。
    pub fn try_from_u32(cmd: u32) -> Option<Self> {
        IoctlCmd::try_from_u32(cmd)
            .map(Self::Modern)
            .or_else(|| LegacyIoctlCmd::try_from_u32(cmd).map(Self::Legacy))
    }

    /// 原始命令号（用于分发器的有效位图索引）。
    pub(crate) fn raw(self) -> u32 {
        match self {
            Self::Modern(c) => c as u32,
            Self::Legacy(c) => c as u32,
        }
    }
}

// ── 分发器（带有效位图的薄封装） ─────────────────────────

/// IOCTL 分发器 — 校验并分发统一命令（[`VideoIoctl`]）。
pub struct IoctlDispatcher {
    valid: [u64; 4],
}

impl IoctlDispatcher {
    pub const fn new() -> Self {
        Self {
            valid: [u64::MAX; 4],
        }
    }

    /// 按原始命令号禁用指定 ioctl 命令。
    pub fn disable_cmd(&mut self, cmd: u32) {
        let idx = (cmd & 0xff) as usize;
        self.valid[idx / 64] &= !(1u64 << (idx % 64));
    }

    fn is_valid(&self, cmd: u32) -> bool {
        let idx = (cmd & 0xff) as usize;
        self.valid[idx / 64] & (1u64 << (idx % 64)) != 0
    }

    pub fn dispatch(
        &self,
        ops: &mut dyn V4L2DriverOps,
        cmd: VideoIoctl,
        arg: &mut [u8],
    ) -> Result<()> {
        if !self.is_valid(cmd.raw()) {
            return Err(V4l2Error::NotSupported);
        }
        match cmd {
            VideoIoctl::Modern(c) => c.dispatch(ops, arg),
            VideoIoctl::Legacy(c) => c.dispatch(ops, arg),
        }
    }
}

impl Default for IoctlDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::{
        dv::{BtTimings, BtTimingsCap},
        legacy::debug::DbgMatch,
    };

    /// Linux UAPI 编码校验：VIDIOC_G_PRIORITY = _IOR('V', 67, __u32) =
    /// 0x80045643、VIDIOC_S_PRIORITY = _IOW('V', 68, __u32) = 0x40045644
    /// （videodev2.h，enum v4l2_priority 为 4 字节）。
    /// 编码不匹配时用户态 ioctl 会走 unknown-cmd 路径（ENOTTY）。
    #[test]
    fn priority_ioctl_codes_match_linux() {
        assert_eq!(IoctlCmd::GPriority as u32, 0x8004_5643);
        assert_eq!(IoctlCmd::SPriority as u32, 0x4004_5644);
        assert_eq!(
            IoctlCmd::try_from_u32(0x8004_5643),
            Some(IoctlCmd::GPriority)
        );
        assert_eq!(
            IoctlCmd::try_from_u32(0x4004_5644),
            Some(IoctlCmd::SPriority)
        );
    }

    /// 无效 ioctl 命令号必须解析为 None（glue 据此返回 ENOTTY，
    /// 对齐 Linux video_ioctl2；v4l2-compliance invalid ioctls 检查）。
    #[test]
    fn unknown_ioctl_is_none() {
        assert_eq!(IoctlCmd::try_from_u32(0xdead_beef), None);
        assert_eq!(IoctlCmd::try_from_u32(0x8004_562D), None); // 表外 nr=45
        assert_eq!(LegacyIoctlCmd::try_from_u32(0xdead_beef), None);
        assert_eq!(VideoIoctl::try_from_u32(0xdead_beef), None);
    }

    /// `VideoIoctl` 归一化：modern 与 legacy 命令都归一到统一枚举；
    /// 弃用的 GCtrl/SCtrl（现属 legacy）不再被当作 modern。
    #[test]
    fn video_ioctl_normalizes_modern_and_legacy() {
        assert_eq!(
            VideoIoctl::try_from_u32(IoctlCmd::QueryCtrl as u32),
            Some(VideoIoctl::Modern(IoctlCmd::QueryCtrl))
        );
        assert_eq!(
            VideoIoctl::try_from_u32(LegacyIoctlCmd::GCtrl as u32),
            Some(VideoIoctl::Legacy(LegacyIoctlCmd::GCtrl))
        );
        // modern 不再包含 GCtrl/SCtrl（已移入 legacy）。
        assert_eq!(IoctlCmd::try_from_u32(LegacyIoctlCmd::GCtrl as u32), None);
        assert_eq!(IoctlCmd::try_from_u32(LegacyIoctlCmd::SCtrl as u32), None);
    }

    /// 补全性检查：83 个命令覆盖全部 VIDIOC 定义，且现代/遗留无重叠。
    #[test]
    fn ioctl_count_matches_linux() {
        assert_eq!(IoctlCmd::COUNT, 47);
        assert_eq!(LegacyIoctlCmd::COUNT, 36);
        assert_eq!(IoctlCmd::COUNT + LegacyIoctlCmd::COUNT, 83);
        for c in IoctlCmd::ALL {
            assert_eq!(IoctlCmd::try_from_u32(c as u32), Some(c));
        }
        for c in LegacyIoctlCmd::ALL {
            assert_eq!(LegacyIoctlCmd::try_from_u32(c as u32), Some(c));
        }
        // 现代/遗留命令号互不重叠。
        for m in IoctlCmd::ALL {
            assert_eq!(LegacyIoctlCmd::try_from_u32(m as u32), None);
        }
        for l in LegacyIoctlCmd::ALL {
            assert_eq!(IoctlCmd::try_from_u32(l as u32), None);
        }
    }

    /// ABI 结构体大小校验：与 C 头文件 sizeof 完全一致（videodev2.h /
    /// v4l2-common.h），保证 ioctl 命令号编码正确。
    #[test]
    fn abi_struct_sizes_match_linux() {
        assert_eq!(size_of::<Framebuffer>(), 48);
        assert_eq!(size_of::<Standard>(), 72);
        assert_eq!(size_of::<Tuner>(), 84);
        assert_eq!(size_of::<Modulator>(), 68);
        assert_eq!(size_of::<Frequency>(), 44);
        assert_eq!(size_of::<FrequencyBand>(), 64);
        assert_eq!(size_of::<HwFreqSeek>(), 48);
        assert_eq!(size_of::<Audio>(), 52);
        assert_eq!(size_of::<AudioOut>(), 52);
        assert_eq!(size_of::<JpegCompression>(), 140);
        assert_eq!(size_of::<SlicedVbiCap>(), 116);
        assert_eq!(size_of::<EncIndex>(), 2072);
        assert_eq!(size_of::<EncoderCmd>(), 40);
        assert_eq!(size_of::<DecoderCmd>(), 72);
        assert_eq!(size_of::<DbgMatch>(), 36);
        assert_eq!(size_of::<DbgRegister>(), 56);
        assert_eq!(size_of::<DbgChipInfo>(), 200);
        assert_eq!(size_of::<BtTimings>(), 124);
        assert_eq!(size_of::<DvTimings>(), 132);
        assert_eq!(size_of::<EnumDvTimings>(), 148);
        assert_eq!(size_of::<BtTimingsCap>(), 104);
        assert_eq!(size_of::<DvTimingsCap>(), 144);
        assert_eq!(size_of::<Edid>(), 40);
    }

    /// 代表性遗留 ioctl 编码与 Linux UAPI 逐一比对（videodev2.h）：
    /// VIDIOC_GCROP = _IOWR('V', 59, v4l2_crop=20) = 0xC014563B；
    /// VIDIOC_GTUNER = _IOWR('V', 29, v4l2_tuner=84) = 0xC054561D；
    /// VIDIOC_G_FBUF = _IOR('V', 10, v4l2_framebuffer=48) = 0x8030564A。
    #[test]
    fn legacy_ioctl_codes_match_linux() {
        assert_eq!(LegacyIoctlCmd::GCrop as u32, 0xC014_563B);
        assert_eq!(LegacyIoctlCmd::GTuner as u32, 0xC054_561D);
        assert_eq!(LegacyIoctlCmd::GFbuf as u32, 0x8030_560A);
        assert_eq!(LegacyIoctlCmd::GJpegComp as u32, 0x808C_563D);
    }
}
