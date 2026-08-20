//! IoctlCmd — V4L2 ioctl 命令枚举及分发。
//!
//! 相当于 Linux 的 `__video_do_ioctl()` + `v4l2_ioctls[]` 分发表。
//! 每个 ioctl 命令是一个 `#[repr(u32)]` 枚举变体，其判别值为与 Linux
//! 兼容的命令号。`dispatch` 方法通过 match 直接映射到
//! `IoctlOps` 回调。

mod ops;

use core::{mem, ptr};

pub use ops::IoctlOps;

use crate::{
    V4l2Error,
    interface::{
        buffer::{Buffer, CreateBuffers, Exportbuffer, RemoveBuffers, Requestbuffers},
        capability::Capability,
        common::BufType,
        crop::{Crop, Cropcap, Selection},
        ctrl::{Control, ExtControls, QueryCtrl, QueryExtCtrl, Querymenu},
        event::{Event, EventSubscription},
        format::{Fmtdesc, Format, FrameIntervalEnum, FrameSizeEnum},
        inout::{Input, Output},
        stream::StreamParm,
    },
};

type Result<T> = core::result::Result<T, V4l2Error>;

// ── IOCTL 代码生成辅助函数 ───────────────────────────────────────

const DIR_WRITE: u32 = 1;
const DIR_READ: u32 = 2;
const DIRSHIFT: u32 = 30;
const VT: u8 = b'V';

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

/// 从扁平的 `(variant, dir, nr, type)` 表生成 [`IoctlCmd`] 枚举、
/// `COUNT` 和 `try_from_u32` — 无需手工维护计数器或 match 分支。
macro_rules! ioctl_defs {
    ($(($variant:ident, $dir:ident, $nr:expr, $ty:ty)),* $(,)?) => {
        /// V4L2 ioctl 命令 — 每个变体对应一个 `VIDIOC_*` 命令。
        ///
        /// 判别值与 Linux ABI 完全一致。
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u32)]
        pub enum IoctlCmd {
            $($variant = ioctl_defs!(@val $dir, $nr, $ty),)*
        }

        impl IoctlCmd {
            /// 已知 ioctl 命令的数量。
            pub const COUNT: usize = [$(stringify!($variant)),*].len();

            /// 尝试将原始 `u32` ioctl 命令号转换为 [`IoctlCmd`]。
            ///
            /// 若命令无法识别则返回 `None`。
            pub fn try_from_u32(cmd: u32) -> Option<Self> {
                Some(match cmd {
                    $(c if c == Self::$variant as u32 => Self::$variant,)*
                    _ => return None,
                })
            }
        }
    };
    (@val ior, $nr:expr, $ty:ty) => { ior($nr, core::mem::size_of::<$ty>() as u32) };
    (@val iow, $nr:expr, $ty:ty) => { iow($nr, core::mem::size_of::<$ty>() as u32) };
    (@val iowr, $nr:expr, $ty:ty) => { iowr($nr, core::mem::size_of::<$ty>() as u32) };
}

ioctl_defs! {
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

    // ── 优先级 ──────────────
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
    (GCtrl, iowr, 27, Control),
    (SCtrl, iowr, 28, Control),
    (GExtCtrls, iowr, 71, ExtControls),
    (SExtCtrls, iowr, 72, ExtControls),
    (TryExtCtrls, iowr, 73, ExtControls),
    (QueryMenu, iowr, 37, Querymenu),

    // ── 裁剪 / Selection ─────────────────────────────────────────
    (CropCap, iowr, 58, Cropcap),
    (GCrop, iowr, 59, Crop),
    (SCrop, iow, 60, Crop),
    (GSelection, iowr, 94, Selection),
    (SSelection, iowr, 95, Selection),

    // ── 事件 ───────────────────────────────────────────────────────
    (DQEvent, ior, 89, Event),
    (SubscribeEvent, iow, 90, EventSubscription),
    (UnsubscribeEvent, iow, 91, EventSubscription),
}

impl IoctlCmd {
    /// 将该 ioctl 命令分发给驱动的 [`IoctlOps`]。
    ///
    /// `arg` 是包含来自用户态的 C 结构的字节切片。
    /// 对 `arg` 的所有读写均使用 `#[repr(C)]` 结构布局。
    #[allow(clippy::too_many_lines)]
    pub fn dispatch(self, ops: &mut dyn IoctlOps, arg: &mut [u8]) -> Result<()> {
        unsafe {
            match self {
                // ── 查询与枚举 ──────────────────────────────
                Self::QueryCap => {
                    let mut cap: Capability = read_from_bytes(arg);
                    ops.querycap(&mut cap)?;
                    write_to_bytes(arg, &cap);
                    Ok(())
                }
                Self::EnumFmt => {
                    let mut f: Fmtdesc = read_from_bytes(arg);
                    ops.enum_fmt(&mut f)?;
                    write_to_bytes(arg, &f);
                    Ok(())
                }
                Self::EnumFrameSizes => {
                    let mut f: FrameSizeEnum = read_from_bytes(arg);
                    ops.enum_framesizes(&mut f)?;
                    write_to_bytes(arg, &f);
                    Ok(())
                }
                Self::EnumFrameIntervals => {
                    let mut f: FrameIntervalEnum = read_from_bytes(arg);
                    ops.enum_frameintervals(&mut f)?;
                    write_to_bytes(arg, &f);
                    Ok(())
                }

                // ── 格式协商 ───────────────────────────────
                Self::GFmt => {
                    let mut f: Format = read_from_bytes(arg);
                    ops.g_fmt(&mut f)?;
                    write_to_bytes(arg, &f);
                    Ok(())
                }
                Self::SFmt => {
                    let mut f: Format = read_from_bytes(arg);
                    ops.s_fmt(&mut f)?;
                    write_to_bytes(arg, &f);
                    Ok(())
                }
                Self::TryFmt => {
                    let mut f: Format = read_from_bytes(arg);
                    ops.try_fmt(&mut f)?;
                    write_to_bytes(arg, &f);
                    Ok(())
                }

                // ── 缓冲区管理 ────────────────────────────────
                Self::ReqBufs => {
                    let mut req: Requestbuffers = read_from_bytes(arg);
                    ops.reqbufs(&mut req)?;
                    write_to_bytes(arg, &req);
                    Ok(())
                }
                Self::QueryBuf => {
                    let mut buf: Buffer = read_from_bytes(arg);
                    ops.querybuf(&mut buf)?;
                    write_to_bytes(arg, &buf);
                    Ok(())
                }
                Self::QBuf => {
                    let mut buf: Buffer = read_from_bytes(arg);
                    ops.qbuf(&mut buf)?;
                    write_to_bytes(arg, &buf);
                    Ok(())
                }
                Self::DQBuf => {
                    let mut buf: Buffer = read_from_bytes(arg);
                    ops.dqbuf(&mut buf)?;
                    write_to_bytes(arg, &buf);
                    Ok(())
                }
                Self::PrepareBuf => {
                    let mut buf: Buffer = read_from_bytes(arg);
                    ops.prepare_buf(&mut buf)?;
                    write_to_bytes(arg, &buf);
                    Ok(())
                }
                Self::CreateBufs => {
                    let mut cb: CreateBuffers = read_from_bytes(arg);
                    ops.create_bufs(&mut cb)?;
                    write_to_bytes(arg, &cb);
                    Ok(())
                }
                Self::RemoveBufs => {
                    let mut rb: RemoveBuffers = read_from_bytes(arg);
                    ops.remove_bufs(&mut rb)?;
                    write_to_bytes(arg, &rb);
                    Ok(())
                }
                Self::ExpBuf => {
                    let mut eb: Exportbuffer = read_from_bytes(arg);
                    ops.expbuf(&mut eb)?;
                    write_to_bytes(arg, &eb);
                    Ok(())
                }

                // ── 流式传输 ────────────────────────────────────────
                Self::StreamOn => {
                    let type_val: u32 = read_from_bytes(arg);
                    let bt = BufType::try_from_u32(type_val).ok_or(V4l2Error::InvalidArgument)?;
                    ops.streamon(bt)
                }
                Self::StreamOff => {
                    let type_val: u32 = read_from_bytes(arg);
                    let bt = BufType::try_from_u32(type_val).ok_or(V4l2Error::InvalidArgument)?;
                    ops.streamoff(bt)
                }

                // ── 流参数 ─────────────────────────────
                Self::GParm => {
                    let mut p: StreamParm = read_from_bytes(arg);
                    ops.g_parm(&mut p)?;
                    write_to_bytes(arg, &p);
                    Ok(())
                }
                Self::SParm => {
                    let p: StreamParm = read_from_bytes(arg);
                    ops.s_parm(&p)
                }

                // GPriority/SPriority 由 VideoDevice::handle_ioctl 拦截
                // （core 层维护优先级，同事件 ioctl），不进驱动分发。
                Self::GPriority | Self::SPriority => Err(V4l2Error::NotSupported),

                // ── 输入/输出选择 ─────────────────────────
                Self::EnumInput => {
                    let mut inp: Input = read_from_bytes(arg);
                    ops.enum_input(&mut inp)?;
                    write_to_bytes(arg, &inp);
                    Ok(())
                }
                Self::GInput => {
                    let idx = ops.g_input()?;
                    write_to_bytes(arg, &idx);
                    Ok(())
                }
                Self::SInput => {
                    let idx: u32 = read_from_bytes(arg);
                    ops.s_input(idx)
                }
                Self::EnumOutput => {
                    let mut out: Output = read_from_bytes(arg);
                    ops.enum_output(&mut out)?;
                    write_to_bytes(arg, &out);
                    Ok(())
                }
                Self::GOutput => {
                    let idx = ops.g_output()?;
                    write_to_bytes(arg, &idx);
                    Ok(())
                }
                Self::SOutput => {
                    let idx: u32 = read_from_bytes(arg);
                    ops.s_output(idx)
                }

                // ── 控制 ─────────────────────────────────────────
                Self::QueryCtrl => {
                    let mut q: QueryCtrl = read_from_bytes(arg);
                    ops.queryctrl(&mut q)?;
                    write_to_bytes(arg, &q);
                    Ok(())
                }
                Self::QueryExtCtrl => {
                    let mut q: QueryExtCtrl = read_from_bytes(arg);
                    ops.query_ext_ctrl(&mut q)?;
                    write_to_bytes(arg, &q);
                    Ok(())
                }
                Self::GCtrl => {
                    let mut c: Control = read_from_bytes(arg);
                    ops.g_ctrl(&mut c)?;
                    write_to_bytes(arg, &c);
                    Ok(())
                }
                Self::SCtrl => {
                    let c: Control = read_from_bytes(arg);
                    ops.s_ctrl(&c)
                }
                Self::QueryMenu => {
                    let mut q: Querymenu = read_from_bytes(arg);
                    ops.querymenu(&mut q)?;
                    write_to_bytes(arg, &q);
                    Ok(())
                }
                // GExtCtrls/SExtCtrls/TryExtCtrls 由 VFS 层拦截处理（需读用户态
                // payload 数组），不进 dispatch；此处仅满足 match 穷尽性。
                Self::GExtCtrls | Self::SExtCtrls | Self::TryExtCtrls => {
                    Err(V4l2Error::NotSupported)
                }

                // ── 裁剪 / Selection ─────────────────────────────
                Self::CropCap => {
                    let mut c: Cropcap = read_from_bytes(arg);
                    ops.cropcap(&mut c)?;
                    write_to_bytes(arg, &c);
                    Ok(())
                }
                Self::GCrop => {
                    let mut c: Crop = read_from_bytes(arg);
                    ops.g_crop(&mut c)?;
                    write_to_bytes(arg, &c);
                    Ok(())
                }
                Self::SCrop => {
                    let c: Crop = read_from_bytes(arg);
                    ops.s_crop(&c)
                }
                Self::GSelection => {
                    let mut s: Selection = read_from_bytes(arg);
                    ops.g_selection(&mut s)?;
                    write_to_bytes(arg, &s);
                    Ok(())
                }
                Self::SSelection => {
                    let s: Selection = read_from_bytes(arg);
                    ops.s_selection(&s)
                }

                // ── 事件 ───────────────────────────────────────────
                // 事件 ioctl 由 VideoDevice::handle_ioctl 拦截并路由到
                // 驱动回调（带 fh），不进此分发器；此处仅满足 match 穷尽性。
                Self::DQEvent | Self::SubscribeEvent | Self::UnsubscribeEvent => {
                    Err(V4l2Error::NotSupported)
                }
            }
        }
    }
}

// ── 分发器（带有效位图的薄封装） ─────────────────────────

/// IOCTL 分发器 — 校验并分发原始 ioctl 命令号。
///
/// 维护有效 ioctl 的位图。可通过 [`disable`](Self::disable) 禁用命令。
/// 分发先经过 [`IoctlCmd::try_from_u32`]，
/// 再调用 [`IoctlCmd::dispatch`]。
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

    /// 将原始 ioctl 命令号分发给驱动。
    pub fn dispatch(&self, ops: &mut dyn IoctlOps, cmd: u32, arg: &mut [u8]) -> Result<()> {
        if !self.is_valid(cmd) {
            return Err(V4l2Error::NotSupported);
        }
        let cmd = IoctlCmd::try_from_u32(cmd).ok_or(V4l2Error::NotSupported)?;
        cmd.dispatch(ops, arg)
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
    }
}
