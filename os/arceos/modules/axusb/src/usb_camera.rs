//! USB UVC 摄像头便捷模块。
//!
//! 基于新的 UVC class driver（[`crate::class::uvc::UvcCamera`]），
//! 提供摄像头初始化 + 帧抓取 + imgcat 输出的端到端流程。
//!
//! # 使用
//!
//! ```ignore
//! // 在 USB 子系统初始化后
//! let mut host = ax_usb::usb_host().lock();
//! host.enumerate()?;
//! let dev = host.open(0)?;
//! let mut cam = UvcCamera::probe(dev)?;
//! let jpeg = cam.capture_frame()?;
//! imgcat::print_image(&jpeg);
//! ```

use alloc::vec::Vec;

use crate::{Device, class::uvc::UvcCamera, imgcat};

/// 初始化 UVC 摄像头并抓取一帧，通过 imgcat 输出 JPEG。
///
/// 这是一个便捷函数，封装了完整的 UVC 流程：
/// 枚举 → 打开 → 协商 → 抓帧 → 图片输出。
pub fn run(device: Device) {
    info!("UVC Camera: probing...");
    let mut cam = match UvcCamera::probe(device) {
        Ok(c) => c,
        Err(e) => {
            error!("UVC Camera probe failed: {:?}", e);
            return;
        }
    };

    info!("UVC Camera: capturing frame...");
    match cam.capture_frame() {
        Ok(jpeg) => {
            info!("UVC Camera: captured {} bytes", jpeg.len());
            imgcat::print_image(&jpeg);
        }
        Err(e) => {
            error!("UVC Camera capture failed: {:?}", e);
        }
    }
}

/// 抓取一帧并返回 JPEG 字节。
pub fn capture_jpeg(cam: &mut UvcCamera) -> Option<Vec<u8>> {
    match cam.capture_frame() {
        Ok(jpeg) => Some(jpeg),
        Err(e) => {
            error!("UVC capture failed: {:?}", e);
            None
        }
    }
}
