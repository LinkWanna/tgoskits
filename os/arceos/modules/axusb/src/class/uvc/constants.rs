//! UVC 常量和全局帧状态。

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

// ── 常量 ──

pub(crate) const VS_PROBE_CONTROL: u8 = 0x01;
pub(crate) const VS_COMMIT_CONTROL: u8 = 0x02;

pub(crate) const USB_DT_CONFIGURATION: u8 = 2;
pub(crate) const USB_DT_INTERFACE: u8 = 4;
pub(crate) const USB_DT_ENDPOINT: u8 = 5;
pub(crate) const CS_INTERFACE: u8 = 0x24;

pub(crate) const VS_FORMAT_MJPEG: u8 = 0x06;
pub(crate) const VS_FRAME_MJPEG: u8 = 0x07;
pub(crate) const VS_FORMAT_UNCOMPRESSED: u8 = 0x04;
pub(crate) const VS_FRAME_UNCOMPRESSED: u8 = 0x05;

pub(crate) const USB_CLASS_VIDEO: u8 = 0x0e;
pub(crate) const USB_SUBCLASS_VIDEO_STREAMING: u8 = 0x02;
pub(crate) const USB_SUBCLASS_VIDEO_CONTROL: u8 = 0x01;

pub(crate) const VC_HEADER: u8 = 0x01;
pub(crate) const VC_INPUT_TERMINAL: u8 = 0x02;
pub(crate) const VC_PROCESSING_UNIT: u8 = 0x05;

pub(crate) const ITT_CAMERA: u16 = 0x0201;

pub(crate) const ENDPOINT_ATTR_ISOCH: u8 = 1;
pub(crate) const ENDPOINT_ATTR_BULK: u8 = 2;

pub(crate) const UVC_PROBE_COMMIT_LEN: usize = 34;

// ── 跨 capture 持久化的帧状态 ──

/// 跨 capture 持久化的「上次 EOF 帧的 FID」。0xFF = 还没抓过。
pub static LAST_EOF_FID: AtomicU8 = AtomicU8::new(0xFF);

/// 重置跨 capture 的连续抓帧状态。
#[inline]
pub fn reset_frame_continuity() {
    LAST_EOF_FID.store(0xFF, Ordering::Relaxed);
}

/// 全局开关：打印微帧级 FID/EOF trace。
pub static FRAME_DEBUG: AtomicBool = AtomicBool::new(false);

/// 像素数上限，0 = 不限制。
pub static PREFERRED_MAX_PIXELS: AtomicU32 = AtomicU32::new(0);

/// 设置 [`PREFERRED_MAX_PIXELS`]。
pub fn set_preferred_max_pixels(p: u32) {
    PREFERRED_MAX_PIXELS.store(p, Ordering::Relaxed);
}
