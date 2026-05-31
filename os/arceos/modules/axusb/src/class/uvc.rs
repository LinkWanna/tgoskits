//! USB Video Class (UVC) 驱动。
//!
//! probe/commit/capture 全部委托给 sg200x-bsp 的已验证实现。
//! DWC2 驱动层保持不变（drivers/usb/dwc2 负责控制+ISOC传输）。

use alloc::vec::Vec;

use ax_driver::prelude::*;

use crate::Device;

// ============================================================================
// UvcCamera
// ============================================================================

/// UVC 摄像头实例。
///
/// probe 使用 sg200x-bsp 的 uvc_start_video_stream，
/// capture 使用 sg200x-bsp 的 uvc_capture_one_frame。
pub struct UvcCamera {
    /// USB 设备地址
    dev_addr: u8,
    /// EP0 最大包长
    ep0_mps: u16,
    /// BSP 流选择（含协商后的 payload/frame size）
    sel: sg200x_bsp::usb::class::uvc::UvcStreamSelection,
    /// 设备句柄（保持设备打开状态）
    device: Device,
}

impl UvcCamera {
    /// 新建 UvcCamera，使用 BSP 的完整 UVC 协商流程。
    ///
    /// 流程：read_config → parse_uvc_video_stream →
    ///       uvc_init_camera_controls → uvc_start_video_stream →
    ///       丢弃首帧。
    pub fn probe(mut device: Device, dev_addr: u8) -> DevResult<Self> {
        use sg200x_bsp::usb::class::uvc;

        // 获取 EP0 MPS（从已枚举的设备描述符）
        let ep0_mps = device.descriptor().max_packet_size_0 as u16;
        if ep0_mps == 0 {
            return Err(DevError::BadState);
        }
        let dev = dev_addr as u32;
        let ep0 = ep0_mps as u32;

        // 1. 读配置描述符
        let cfg_buf = uvc::read_configuration_descriptor(dev, ep0, 1).map_err(|e| {
            error!("UVC: read_configuration_descriptor err={:?}", e);
            DevError::Io
        })?;
        let cfg_total = u16::from_le_bytes([cfg_buf[2], cfg_buf[3]]) as usize;

        // 2. 解析 UVC 流参数
        let mut sel =
            uvc::parse_uvc_video_stream(&cfg_buf[..cfg_total.min(cfg_buf.len())], cfg_total)
                .map_err(|e| {
                    error!("UVC: parse_uvc_video_stream err={:?}", e);
                    DevError::Io
                })?;

        // 3. 初始化相机控制（brightness 等）
        if let Some(entities) =
            uvc::parse_uvc_control_entities(&cfg_buf[..cfg_total.min(cfg_buf.len())], cfg_total)
        {
            let tune = sg200x_bsp::usb::class::uvc::UvcImageTuning {
                brightness: Some(96),
                contrast: None,
                hue: None,
                saturation: None,
                sharpness: None,
                gamma: None,
                backlight: None,
                gain: None,
                white_balance_temp_k: None,
                power_line_freq: None,
            };
            let _ = uvc::uvc_init_camera_controls(dev, ep0, &entities, &tune);
        }

        // 4. 启动视频流（PROBE → GET_CUR → COMMIT → SET_INTERFACE）
        uvc::uvc_start_video_stream(dev, ep0, &mut sel).map_err(|e| {
            error!("UVC: uvc_start_video_stream err={:?}", e);
            DevError::Io
        })?;
        info!(
            "UVC: stream ready {}x{} payload={} frame_size={}",
            sel.frame_w, sel.frame_h, sel.negotiated_payload_size, sel.negotiated_frame_size
        );

        // 5. 丢弃首帧（相机流启动后的初始短帧）
        let _ = uvc::uvc_capture_one_frame(dev, ep0, &sel);

        Ok(Self {
            dev_addr,
            ep0_mps,
            sel,
            device,
        })
    }

    /// 捕获一帧 MJPEG 数据。
    ///
    /// 内部调用 BSP 的 uvc_capture_one_frame（完整多微帧组装+FID跟踪）。
    /// 无效帧自动重试最多 8 次。
    pub fn capture_frame(&mut self) -> DevResult<Vec<u8>> {
        use sg200x_bsp::usb::class::uvc;

        let dev = self.dev_addr as u32;
        let ep0 = self.ep0_mps as u32;

        const MAX_TRIES: u32 = 8;
        const MIN_VALID_BYTES: usize = 4096;

        for attempt in 0..MAX_TRIES {
            let n = uvc::uvc_capture_one_frame(dev, ep0, &self.sel).map_err(|e| {
                error!(
                    "UVC: capture err={:?} (try {}/{})",
                    e,
                    attempt + 1,
                    MAX_TRIES
                );
                DevError::Io
            })?;

            if n < MIN_VALID_BYTES {
                warn!(
                    "UVC: frame too small (try {}/{}, size={})",
                    attempt + 1,
                    MAX_TRIES,
                    n
                );
                uvc::reset_frame_continuity();
                continue;
            }

            // 从 BSP DMA 缓冲区读取 JPEG 数据
            let s =
                sg200x_bsp::usb::host::dwc2::ep0::dma_rx_slice(uvc::UVC_ASSEMBLED_JPEG_DMA_OFF, n)
                    .ok_or(DevError::Io)?;

            // 验证 JPEG 标记
            if s.len() >= 2
                && s[0] == 0xFF
                && s[1] == 0xD8
                && s[s.len() - 2] == 0xFF
                && s[s.len() - 1] == 0xD9
            {
                info!("UVC: captured {} bytes", s.len());
                return Ok(s.to_vec());
            }

            warn!(
                "UVC: invalid JPEG markers (try {}/{}, size={})",
                attempt + 1,
                MAX_TRIES,
                s.len()
            );
            uvc::reset_frame_continuity();
        }

        Err(DevError::Io)
    }

    /// 获取协商后的帧大小（字节）。
    pub fn frame_size(&self) -> u32 {
        self.sel.negotiated_frame_size
    }

    /// 获取协商后的单微帧负载大小（字节）。
    pub fn payload_size(&self) -> u32 {
        self.sel.negotiated_payload_size
    }

    /// 获取底层设备引用。
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// 获取底层设备可变引用。
    pub fn device_mut(&mut self) -> &mut Device {
        &mut self.device
    }
}
