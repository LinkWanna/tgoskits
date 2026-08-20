//! 模拟电视标准结构（VIDIOC_ENUMSTD）— 对应 Linux `struct v4l2_standard`。
//!
//! 模拟电视标准接口（G/S_STD、ENUMSTD、QUERYSTD）属于模拟电视时代；
//! 现代 HDMI/DP 设备改用 DV timings 接口。

use crate::interface::{common::Fract, inout::StdId};

/// 视频标准描述。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Standard {
    pub index: u32,         // [in] 标准索引
    pub id: StdId,          // [out] 视频标准 ID
    pub name: [u8; 24],     // [out] 标准名称
    pub frameperiod: Fract, // [out] 帧周期（帧，非场）
    pub framelines: u32,    // [out] 每帧行数
    pub reserved: [u32; 4],
}
