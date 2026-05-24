//! SG2002 USB UVC 摄像头一体化模块。
//!
//! 将 SG2002 平台 USB 主机初始化（时钟、PHY、VBUS）、DWC2 控制器探测、
//! UVC 摄像头枚举/协商/抓帧集中在一个文件中。
//!
//! **移植指南**：拷贝本文件到新 ArceOS 工程，声明 `mod usb_camera;`，
//! 然后在 `main()` 中调用 `usb_camera::run()` 即可。
//! 也可使用 `init()` + `capture_frame()` 进行细粒度控制。
//!
//! sg200x-bsp 中的 USB/UVC 协议层代码**不**在本文件中复制，保持原样依赖。

#![allow(dead_code)]

use ax_plat::mem::{PhysAddr, VirtAddr, phys_to_virt, virt_to_phys};
use sg200x_bsp::usb::{
    class::uvc,
    host::{UvcEnumerated, dwc2::ep0 as dwc2_ep0},
};
use tock_registers::interfaces::Writeable;

// =========================================================================
//  帧抓取
// =========================================================================

/// 抓取 1 帧 MJPEG，返回 DMA 缓冲区中的 JPEG 字节切片。
/// 切片在下一次调用 `capture_frame` 之前有效。
/// 内部对 SOI/EOI 做校验，无效帧会自动重试（最多 8 次）。
pub fn capture_frame(
    cam: &UvcEnumerated,
    sel: &uvc::UvcStreamSelection,
) -> Result<&'static [u8], &'static str> {
    let dev = u32::from(cam.addr);
    let ep0 = cam.ep0_mps;

    const MAX_TRIES: u32 = 8;
    const MIN_VALID_BYTES: usize = 4096;
    let mut last_n: usize = 0;
    let mut last_msg: Option<&'static str> = None;
    for attempt in 0..MAX_TRIES {
        let n = uvc::uvc_capture_one_frame(dev, ep0, sel).map_err(|e| {
            debug!("UVC: capture err={:?}", e);
            "抓帧失败"
        })?;
        last_n = n;
        let s = dwc2_ep0::dma_rx_slice(uvc::UVC_ASSEMBLED_JPEG_DMA_OFF, n).ok_or("DMA 切片越界")?;
        let starts_jpeg = n >= 2 && s[0] == 0xff && s[1] == 0xd8;
        let ends_jpeg = n >= 2 && s[n - 2] == 0xff && s[n - 1] == 0xd9;
        if starts_jpeg && ends_jpeg && n >= MIN_VALID_BYTES {
            return Ok(s);
        }
        last_msg = Some(if !starts_jpeg {
            "首字节非 ff d8"
        } else if !ends_jpeg {
            "末字节非 ff d9（被截断）"
        } else {
            "尺寸过小"
        });
        debug!(
            "UVC: 帧无效 (try #{}/{}, size={}, {}); 重置 FID",
            attempt + 1,
            MAX_TRIES,
            n,
            last_msg.unwrap_or("?")
        );
        uvc::reset_frame_continuity();
    }
    debug!(
        "UVC: 重试 {} 次仍未拿到完整 JPEG，size={} {}",
        MAX_TRIES,
        last_n,
        last_msg.unwrap_or("?")
    );
    dwc2_ep0::dma_rx_slice(uvc::UVC_ASSEMBLED_JPEG_DMA_OFF, last_n).ok_or("DMA 切片越界")
}
