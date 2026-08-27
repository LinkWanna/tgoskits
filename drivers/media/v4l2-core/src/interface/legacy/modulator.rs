//! 调制器结构 — 对应 Linux `struct v4l2_modulator`。
//!
//! 调制器接口（G/S_MODULATOR）用于模拟视频输出调制，新设备不再实现。

use crate::interface::legacy::tuner::{TunerCap, TunerType};

/// 调制器。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Modulator {
    pub index: u32,           // [in] 调制器索引
    pub name: [u8; 32],       // [out] 名称
    pub capability: TunerCap, // [out] 调制器能力
    pub rangelow: u32,        // [out] 最低频率
    pub rangehigh: u32,       // [out] 最高频率
    pub txsubchans: u32,      // [out] 发送子信道
    pub ty: TunerType,        // [out] 调谐器类型
    pub reserved: [u32; 3],
}
