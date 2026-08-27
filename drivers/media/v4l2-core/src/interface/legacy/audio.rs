//! 音频输入/输出结构 — 对应 Linux `struct v4l2_audio` / `v4l2_audioout`。
//!
//! 音频 I/O 接口（G/S/ENUM_AUDIO、G/S/ENUM_AUDOUT）已被 ALSA 取代，
//! 属于遗留接口，新设备不再实现。

use bitflags::bitflags;

/// 音频输入。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Audio {
    pub index: u32,           // [in] 音频输入索引
    pub name: [u8; 32],       // [out] 名称
    pub capability: AudioCap, // [out] 音频能力
    pub mode: u32,            // [inout] 音频模式
    pub reserved: [u32; 2],
}

/// 音频输出。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AudioOut {
    pub index: u32,           // [in] 音频输出索引
    pub name: [u8; 32],       // [out] 名称
    pub capability: AudioCap, // [out] 音频能力
    pub mode: u32,            // [inout] 音频模式
    pub reserved: [u32; 2],
}

// ── 音频能力标志 ───────────────────────────────────────────────────

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AudioCap: u32 {
        const STEREO = 0x0001;
        const AVL    = 0x0002;
    }
}
