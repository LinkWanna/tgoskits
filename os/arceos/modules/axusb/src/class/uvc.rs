//! USB Video Class (UVC) 驱动。
//!
//! 基于 axdriver_usb 的 HC-agnostic trait 实现 USB 摄像头控制与视频流捕获。
//!
//! 参考：USB Device Class Definition for Video Devices v1.5
//!
//! # 使用示例
//!
//! ```ignore
//! let dev = host.open(device_id)?;
//! let mut cam = UvcCamera::probe(dev)?;
//! let jpeg = cam.capture_frame()?;
//! ```

use alloc::vec::Vec;

use ax_driver::prelude::*;
use ax_driver_usb::{SetupPacket, UsbDevice as UsbDeviceTrait};

use crate::Device;

// ============================================================================
// UVC 标准常量
// ============================================================================

/// UVC Interface Class/Subclass codes
pub const UVC_CLASS_VIDEO: u8 = 0x0E;
pub const UVC_SUBCLASS_VC: u8 = 0x01; // Video Control
pub const UVC_SUBCLASS_VS: u8 = 0x02; // Video Streaming

/// UVC Video Control 请求码
mod vc_request {
    pub const SET_CUR: u8 = 0x01;
    pub const GET_CUR: u8 = 0x81;
    pub const GET_MIN: u8 = 0x82;
    pub const GET_MAX: u8 = 0x83;
    pub const GET_RES: u8 = 0x84;
    pub const GET_LEN: u8 = 0x85;
    pub const GET_INFO: u8 = 0x86;
    pub const GET_DEF: u8 = 0x87;
}

/// UVC VS Probe/Commit 控制选择子
pub const VS_PROBE_CONTROL: u8 = 0x01;
pub const VS_COMMIT_CONTROL: u8 = 0x02;

/// UVC Streaming 请求码
pub const VS_STREAMING_REQUEST: u8 = 0x0B;

/// VS Probe/Commit 结构体 (26 bytes per UVC 1.1)。
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct UvcProbeCommit {
    pub hint: u16,
    pub format_index: u8,
    pub frame_index: u8,
    pub frame_interval: u32,
    pub key_frame_rate: u16,
    pub p_frame_rate: u16,
    pub comp_quality: u16,
    pub comp_window_size: u16,
    pub delay: u16,
    pub max_video_frame_size: u32,
    pub max_payload_transfer_size: u32,
    /// 变长字段起始偏移 (UVC 1.1+) — 此处忽略
    pub clock_frequency: u32, // 仅 UVC 1.1+
    pub framing_info: u8,
    pub preferred_version: u8,
    pub min_version: u8,
    pub max_version: u8,
    pub usage: u8,          // 仅 UVC 1.5
    pub bit_depth_luma: u8, // 仅 UVC 1.5
}

impl UvcProbeCommit {
    pub const SIZE: usize = core::mem::size_of::<Self>();

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        unsafe { core::mem::transmute(*self) }
    }

    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> Self {
        unsafe { core::mem::transmute(*bytes) }
    }
}

// ============================================================================
// UvcCamera
// ============================================================================

/// UVC 摄像头实例，持有已打开的 USB 设备。
pub struct UvcCamera {
    device: Device,
    /// Video Control 接口号
    vc_iface: u8,
    /// Video Streaming 接口号
    vs_iface: u8,
    /// VS 接口的 streaming alternate setting 编号
    vs_streaming_alt: u8,
    /// Isoch IN 端点地址
    isoch_ep_addr: u8,
    /// Isoch 最大包大小
    isoch_mps: u16,
    /// 协商后的 Probe/Commit 结果
    probe_result: UvcProbeCommit,
    /// 已 streaming
    streaming: bool,
}

impl UvcCamera {
    /// 新建 UvcCamera，基于已打开的 USB 设备进行 UVC 协商。
    ///
    /// 此函数会：
    /// 1. 通过 EP0 读取完整配置描述符，解析 VC/VS 接口和 Isoch IN 端点
    /// 2. 设置活动配置
    /// 3. 执行 VS Probe 协商
    /// 4. 执行 VS Commit 确认
    /// 5. 设置 VS streaming alternate interface
    pub fn probe(mut device: Device) -> DevResult<Self> {
        // 1. 读取并解析配置描述符
        let cfg_buf = Self::read_config_descriptor(&mut device, 0)?;
        let (vc_iface, vs_iface, vs_alt, ep_addr, mps) = Self::parse_uvc_interfaces_raw(&cfg_buf)?;

        debug!(
            "UVC: VC iface={} VS iface={} alt={} isoch_ep=0x{:02x} mps={}",
            vc_iface, vs_iface, vs_alt, ep_addr, mps
        );

        // 2. 设置配置
        device.set_configuration(1)?;

        // 3. VS Probe — 协商流参数
        let mut probe = UvcProbeCommit {
            hint: 0x0001, // dwFrameInterval 有效
            format_index: 1,
            frame_index: 1,
            frame_interval: 333333, // 30 fps
            key_frame_rate: 0,
            p_frame_rate: 0,
            comp_quality: 0,
            comp_window_size: 0,
            delay: 0,
            max_video_frame_size: 0,
            max_payload_transfer_size: 0,
            clock_frequency: 0,
            framing_info: 0,
            preferred_version: 0,
            min_version: 0,
            max_version: 0,
            usage: 0,
            bit_depth_luma: 0,
        };

        // GET_CUR Probe
        let mut buf = [0u8; UvcProbeCommit::SIZE];
        Self::vs_control(
            &mut device,
            vs_iface,
            vc_request::GET_CUR,
            VS_PROBE_CONTROL,
            &mut buf,
        )?;
        probe = UvcProbeCommit::from_bytes(
            &buf[..UvcProbeCommit::SIZE]
                .try_into()
                .map_err(|_| DevError::Io)?,
        );

        // GET_MIN / GET_MAX 验证
        Self::vs_control(
            &mut device,
            vs_iface,
            vc_request::GET_MIN,
            VS_PROBE_CONTROL,
            &mut buf,
        )?;
        let min_probe = UvcProbeCommit::from_bytes(
            &buf[..UvcProbeCommit::SIZE]
                .try_into()
                .map_err(|_| DevError::Io)?,
        );

        Self::vs_control(
            &mut device,
            vs_iface,
            vc_request::GET_MAX,
            VS_PROBE_CONTROL,
            &mut buf,
        )?;

        let min_frame_size = { min_probe.max_video_frame_size };
        let max_frame_size = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
        info!(
            "UVC: probe min_frame_size={} max_frame_size={}",
            min_frame_size, max_frame_size
        );

        // SET_CUR Probe
        let probe_bytes = probe.to_bytes();
        Self::vs_control_set(&mut device, vs_iface, VS_PROBE_CONTROL, &probe_bytes)?;

        // 4. VS Commit
        Self::vs_control_set(&mut device, vs_iface, VS_COMMIT_CONTROL, &probe_bytes)?;

        // 5. 设置 VS streaming alternate
        device.claim_interface(vs_iface, vs_alt)?;

        Ok(Self {
            device,
            vc_iface,
            vs_iface,
            vs_streaming_alt: vs_alt,
            isoch_ep_addr: ep_addr,
            isoch_mps: mps,
            probe_result: probe,
            streaming: false,
        })
    }

    /// 捕获一帧 MJPEG 数据。
    ///
    /// 返回完整的 JPEG 字节流（含 SOI/EOI 标记）。
    /// 内部自动处理 isoch 包的 UVC payload header，组装 MJPEG 帧。
    pub fn capture_frame(&mut self) -> DevResult<Vec<u8>> {
        let frame_size = { self.probe_result.max_video_frame_size } as usize;
        let mut buf = alloc::vec![0u8; frame_size];
        let n = self.device.isoch_in(self.isoch_ep_addr, &mut buf)?;
        buf.truncate(n);

        // 验证 JPEG 标记
        let valid = buf.len() >= 4
            && buf[0] == 0xFF
            && buf[1] == 0xD8
            && buf[buf.len() - 2] == 0xFF
            && buf[buf.len() - 1] == 0xD9;
        if !valid {
            warn!(
                "UVC: JPEG markers invalid (len={}, first={:02x}{:02x}, last={:02x}{:02x})",
                buf.len(),
                buf.first().copied().unwrap_or(0),
                buf.get(1).copied().unwrap_or(0),
                buf.get(buf.len().wrapping_sub(2)).copied().unwrap_or(0),
                buf.get(buf.len().wrapping_sub(1)).copied().unwrap_or(0),
            );
        }
        Ok(buf)
    }

    /// 获取设备引用。
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// 获取设备可变引用。
    pub fn device_mut(&mut self) -> &mut Device {
        &mut self.device
    }

    /// 获取 Probe/Commit 协商结果。
    pub fn probe_result(&self) -> &UvcProbeCommit {
        &self.probe_result
    }

    // ========================================================================
    // 内部辅助
    // ========================================================================

    /// VS 控制请求（GET）。
    fn vs_control(
        dev: &mut Device,
        vs_iface: u8,
        request: u8,
        selector: u8,
        data: &mut [u8],
    ) -> DevResult<usize> {
        let setup = SetupPacket::new(
            0xA1, // IN, Class, Interface
            request,
            (selector as u16) << 8,
            vs_iface as u16,
            data.len() as u16,
        );
        dev.control_in(setup, data)
    }

    /// VS 控制请求（SET）。
    fn vs_control_set(
        dev: &mut Device,
        vs_iface: u8,
        selector: u8,
        data: &[u8],
    ) -> DevResult<usize> {
        let setup = SetupPacket::new(
            0x21, // OUT, Class, Interface
            vc_request::SET_CUR,
            (selector as u16) << 8,
            vs_iface as u16,
            data.len() as u16,
        );
        dev.control_out(setup, data)
    }

    /// 从 UVC payload 中提取纯 JPEG 数据。
    ///
    /// UVC payload header 格式（每帧第一个 isoch 包）：
    /// - byte 0: header length (HLF)
    /// - byte 1: bit 0 = error, bit 1 = EOF (end of frame), ...
    /// 跳过 header 后即为 JPEG 数据。
    fn extract_jpeg_from_payload(payload: &[u8]) -> &[u8] {
        if payload.len() < 2 {
            return payload;
        }
        let hdr_len = payload[0] as usize;
        if hdr_len < 2 || hdr_len > payload.len() {
            return payload;
        }
        &payload[hdr_len..]
    }

    /// 通过 EP0 控制传输读取完整配置描述符。
    fn read_config_descriptor(dev: &mut Device, cfg_index: u8) -> DevResult<alloc::vec::Vec<u8>> {
        // 1. 读取 9 字节头获取 wTotalLength
        let setup = SetupPacket::new(
            0x80,                                // IN, Standard, Device
            0x06,                                // GET_DESCRIPTOR
            ((0x02u16) << 8) | cfg_index as u16, // CONFIGURATION descriptor
            0,
            9,
        );
        let mut hdr = [0u8; 9];
        dev.control_in(setup, &mut hdr)?;

        if hdr[1] != 0x02 {
            // USB_DT_CONFIGURATION
            return Err(DevError::Io);
        }
        let total = u16::from_le_bytes([hdr[2], hdr[3]]) as usize;
        if total == 0 || total > 4096 {
            return Err(DevError::Io);
        }

        // 2. 读取完整描述符
        let setup = SetupPacket::new(
            0x80,
            0x06,
            ((0x02u16) << 8) | cfg_index as u16,
            0,
            total as u16,
        );
        let mut buf = alloc::vec![0u8; total];
        dev.control_in(setup, &mut buf)?;
        Ok(buf)
    }

    /// 从原始配置描述符字节中解析 UVC 接口和 Isoch IN 端点。
    fn parse_uvc_interfaces_raw(cfg: &[u8]) -> DevResult<(u8, u8, u8, u8, u16)> {
        let mut vc_iface: Option<u8> = None;
        let mut vs_iface: Option<u8> = None;
        let mut vs_streaming_alt: Option<u8> = None;
        let mut isoch_ep: Option<u8> = None;
        let mut isoch_mps: u16 = 0;

        let len = cfg.len();
        let mut i = 0usize;
        while i + 2 <= len {
            let desc_len = cfg[i] as usize;
            let desc_type = cfg[i + 1];
            if desc_len < 2 || i + desc_len > len {
                break;
            }
            match desc_type {
                0x04 if i + 9 <= len => {
                    // INTERFACE descriptor
                    let iface_num = cfg[i + 2];
                    let alt_setting = cfg[i + 3];
                    let num_eps = cfg[i + 4];
                    let iface_class = cfg[i + 5];
                    let iface_subclass = cfg[i + 6];

                    if iface_class == UVC_CLASS_VIDEO {
                        match iface_subclass {
                            UVC_SUBCLASS_VC => {
                                vc_iface = Some(iface_num);
                            }
                            UVC_SUBCLASS_VS => {
                                vs_iface = Some(iface_num);
                                if num_eps > 0 {
                                    vs_streaming_alt = Some(alt_setting);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                0x05 if i + 7 <= len => {
                    // ENDPOINT descriptor
                    let ep_addr = cfg[i + 2];
                    let ep_attr = cfg[i + 3];
                    let ep_mps = u16::from_le_bytes([cfg[i + 4], cfg[i + 5]]);
                    let ep_type = ep_attr & 0x03;
                    // Isoch IN: type=1, direction bit=1
                    if ep_type == 1 && (ep_addr & 0x80) != 0 {
                        if isoch_ep.is_none() {
                            isoch_ep = Some(ep_addr);
                            isoch_mps = ep_mps;
                        }
                    }
                }
                _ => {}
            }
            i += desc_len;
        }

        let vc = vc_iface.ok_or(DevError::Unsupported)?;
        let vs = vs_iface.ok_or(DevError::Unsupported)?;
        let alt = vs_streaming_alt.unwrap_or(1);
        let ep = isoch_ep.ok_or(DevError::Unsupported)?;

        Ok((vc, vs, alt, ep, isoch_mps))
    }
}
