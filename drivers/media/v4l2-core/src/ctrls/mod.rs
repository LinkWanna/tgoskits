//! V4L2 控件框架——对应 Linux 的 `v4l2-ctrls-core.c` 与 `v4l2-ctrls-api.c`。
//!
//! - [`class`]：控件类定义（对应 `include/uapi/linux/v4l2-controls.h`），
//!   每个控件类一个独立模块。
//! - [`handler`]：`CtrlHandler` 控件处理器——以 `G/S/TryExtCtrls` 为主线，
//!   `G_CTRL`/`S_CTRL` 作为弃用兼容路径。

mod api;
pub mod class;
pub mod handler;

use alloc::boxed::Box;
use core::sync::atomic::{AtomicI64, Ordering};

pub use handler::*;

use crate::{Result, V4l2Error, interface::ctrl::CtrlFlags};

// ── 控件类型 ─────────────────────────────────────────────────────────

/// V4L2 控件类型
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlType {
    Integer   = 1,
    Boolean   = 2,
    Menu      = 3,
    Button    = 4,
    Integer64 = 5,
    CtrlClass = 6,
    Bitmask   = 8,
}

impl CtrlType {
    /// 由原始 `v4l2_ctrl_type` 数值转换（仅支持当前标量子集）。
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            1 => Self::Integer,
            2 => Self::Boolean,
            3 => Self::Menu,
            4 => Self::Button,
            5 => Self::Integer64,
            6 => Self::CtrlClass,
            8 => Self::Bitmask,
            _ => return None,
        })
    }

    pub fn is_int(&self) -> bool {
        *self != CtrlType::Integer64
    }

    pub fn size(&self) -> u32 {
        if *self == CtrlType::Integer64 { 8 } else { 4 }
    }

    pub fn check_range(&self, min: i64, max: i64, step: u64, def: i64) -> Result<()> {
        match self {
            CtrlType::Boolean => {
                if step != 1 || max > 1 || min < 0 {
                    return Err(V4l2Error::OutOfRange);
                }
                if step == 0 || min > max || def < min || def > max {
                    return Err(V4l2Error::OutOfRange);
                }
                Ok(())
            }
            CtrlType::Integer | CtrlType::Integer64 => {
                if step == 0 || min > max || def < min || def > max {
                    return Err(V4l2Error::OutOfRange);
                }
                Ok(())
            }
            CtrlType::Bitmask => {
                if step != 0 || min != 0 || max == 0 || (def & !max) != 0 {
                    return Err(V4l2Error::OutOfRange);
                }
                Ok(())
            }
            CtrlType::Menu => {
                if min > max || def < min || def > max || min < 0 || (step != 0 && max >= 64) {
                    return Err(V4l2Error::OutOfRange);
                }
                if def < 64 && (step & (1u64 << def)) != 0 {
                    return Err(V4l2Error::InvalidArgument);
                }
                Ok(())
            }
            CtrlType::Button | CtrlType::CtrlClass => Ok(()),
        }
    }
}

// ── 控件回调 ─────────────────────

pub type CtrlGetFn = Box<dyn Fn() -> Result<i64> + Send + Sync>;
pub type CtrlTryFn = Box<dyn Fn(i64) -> Result<i64> + Send + Sync>;
pub type CtrlSetFn = Box<dyn Fn(i64) -> Result<i64> + Send + Sync>;

/// 控件硬件回调集合。
pub struct CtrlOps {
    pub get: Option<CtrlGetFn>,
    pub try_ctrl: Option<CtrlTryFn>,
    pub set: CtrlSetFn,
}

// ── 控件配置 ───────────────────

/// 注册控件所需的完整配置。
pub struct CtrlConfig {
    pub id: u32,
    pub name: &'static str,
    pub ctrl_type: CtrlType,
    pub minimum: i64,
    pub maximum: i64,
    pub step: u64,
    pub default_value: i64,
    pub flags: CtrlFlags,
    pub qmenu: Option<&'static [&'static str]>,
    pub ops: Option<CtrlOps>,
}

// ── 控件 ─────────────────────────────────────────────────────────────

/// 已注册的控件，包含元数据与当前值。
pub struct Ctrl {
    pub id: u32,
    pub name: &'static str,
    pub ctrl_type: CtrlType,
    pub minimum: i64,
    pub maximum: i64,
    pub step: u64,
    pub default_value: i64,
    pub flags: CtrlFlags,
    pub qmenu: Option<&'static [&'static str]>,
    pub(crate) ops: Option<CtrlOps>,
    cur: AtomicI64,
}

impl Ctrl {
    /// 当前值（无锁读取，供驱动填充 / 采样路径使用）。
    pub fn value(&self) -> i64 {
        self.cur.load(Ordering::Acquire)
    }

    fn set_value(&self, v: i64) {
        self.cur.store(v, Ordering::Release);
    }
}
