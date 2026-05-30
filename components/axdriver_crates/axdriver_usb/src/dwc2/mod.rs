//! DWC2 USB 主机控制器后端。
//!
//! 封装 sg200x-bsp 的 DWC2 驱动，实现 axdriver_usb 的 HC-agnostic trait：
//! - [`UsbHostController`]：控制器初始化 + 设备枚举
//! - [`UsbDevice`]：设备句柄 + 传输
//!
//! 同时包含 SG2002 平台初始化（PHY + pinctrl + VBUS），
//! 通过 [`platform_init`] 统一入口。

pub mod phy;
pub mod pinctrl;

use alloc::{boxed::Box, vec, vec::Vec};

use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use ax_hal::mem::{VirtAddr, virt_to_phys};
use sg200x_bsp::usb::{
    host::{self, dwc2},
    log, platform,
};

use crate::{
    SetupPacket, UsbDevice, UsbHostController,
    device::{ConfigurationDescriptor, DeviceDescriptor},
};

// ============================================================================
// 平台初始化
// ============================================================================

/// SG2002 cv181x USB 平台初始化：时钟 + PHY + pinctrl + VBUS + 延时等 VBUS 稳定。
///
/// # Safety
///
/// 必须在 MMU 启用后、单核初始化阶段调用。
pub unsafe fn platform_init() {
    unsafe {
        phy::enable_usb_clocks_cv181x();
        phy::cvitek_usb_top_host_bringup();
    }
    pinctrl::pinmux_usb_vbus_det_gpio_output_prep();
    pinctrl::enable_usb_vbus_gpio();
    // 等待 VBUS 稳定约 2 秒（经验值，USB 规范要求 ≥100ms）
    for _ in 0..2_000_000u32 {
        core::hint::spin_loop();
    }
}

// ============================================================================
// 辅助
// ============================================================================

#[inline]
fn spin_udelay_approx(us: u32) {
    for _ in 0..us.saturating_mul(64) {
        core::hint::spin_loop();
    }
}

fn ep0_dma_virt_to_phys(p: *const u8) -> u32 {
    virt_to_phys(VirtAddr::from(p as usize)).as_usize() as u32
}

fn usb_log_line(s: &str) {
    debug!("{s}");
}

/// 将 crate::SetupPacket 转为 sg200x-bsp 的 `[u8; 8]` SETUP 包。
fn setup_to_raw(sp: &SetupPacket) -> [u8; 8] {
    let mut raw = [0u8; 8];
    raw[0] = sp.request_type;
    raw[1] = sp.request;
    raw[2] = sp.value as u8;
    raw[3] = (sp.value >> 8) as u8;
    raw[4] = sp.index as u8;
    raw[5] = (sp.index >> 8) as u8;
    raw[6] = sp.length as u8;
    raw[7] = (sp.length >> 8) as u8;
    raw
}

/// 通用 bulk/isoch 传输用的 DMA 偏移（复用 UVC bulk 区域）。
const BULK_DMA_OFF: usize = dwc2::ep0::DMA_OFF_UVC_BULK;

// ============================================================================
// Dwc2 — 设备描述符持有者（供 axdriver 设备注册使用）
// ============================================================================

/// DWC2 设备描述符。
///
/// 由 axdriver 在设备探测阶段创建，持有的 `dwc2_base` 供 axusb::init_usb
/// 提取并构造 [`Dwc2HostController`]。
pub struct Dwc2 {
    dwc2_base: usize,
}

impl Dwc2 {
    pub fn new(dwc2_base: usize) -> Self {
        Self { dwc2_base }
    }

    /// 返回 DWC2 寄存器 MMIO 虚拟基址。
    pub fn base(&self) -> usize {
        self.dwc2_base
    }
}

impl BaseDriverOps for Dwc2 {
    fn device_name(&self) -> &str {
        "DWC2 USB Host Controller"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Usb
    }
}

// ============================================================================
// Dwc2HostController — UsbHostController 实现
// ============================================================================

/// DWC2 主机控制器。
pub struct Dwc2HostController {
    dwc2_base: usize,
}

impl Dwc2HostController {
    pub fn new(dwc2_base: usize) -> Self {
        Self { dwc2_base }
    }
}

impl UsbHostController for Dwc2HostController {
    fn init(&mut self) -> DevResult<()> {
        unsafe {
            platform_init();
        }

        platform::set_dwc2_base_virt(self.dwc2_base);
        platform::set_usb_dma_to_phys_fn(Some(ep0_dma_virt_to_phys));
        log::set_usb_log_fn(usb_log_line);
        dwc2::ep0::debug_log_ep0_dma_info();

        unsafe {
            dwc2::dwc2_probe().map_err(|e| {
                error!("USB DWC2 probe failed: {e:?}");
                DevError::Io
            })?;
        }

        info!("DWC2 host controller initialized");
        Ok(())
    }

    fn probe(&mut self) -> DevResult<Vec<crate::ProbedDeviceInfo>> {
        let mut last_err = None;
        let extras = (0..4)
            .find_map(|attempt| {
                if attempt > 0 {
                    spin_udelay_approx(1_500_000 * attempt as u32);
                }
                match host::enumerate_topology_only() {
                    Ok(ex) => Some(ex),
                    Err(e) => {
                        warn!("USB: 枚举失败 #{}: {:?}", attempt + 1, e);
                        last_err = Some(e);
                        None
                    }
                }
            })
            .ok_or_else(|| {
                warn!("USB: 枚举重试全部失败: {:?}", last_err);
                DevError::Unsupported
            })?;

        let mut devices = Vec::new();

        if let Some(uvc) = extras.uvc {
            let desc = DeviceDescriptor {
                usb_version: 0x0200,
                class: 0xEF,
                subclass: 0x02,
                protocol: 0x01,
                max_packet_size_0: uvc.ep0_mps as u8,
                vendor_id: uvc.vid,
                product_id: uvc.pid,
                device_version: 0,
                manufacturer_str_idx: 0,
                product_str_idx: 0,
                serial_str_idx: 0,
                num_configurations: 1,
            };

            let config = ConfigurationDescriptor {
                total_length: 0,
                num_interfaces: 1,
                configuration_value: 1,
                config_str_idx: 0,
                attributes: 0x80,
                max_power: 50,
                interfaces: Vec::new(),
            };

            devices.push(crate::ProbedDeviceInfo {
                device_id: uvc.addr as usize,
                descriptor: desc,
                config_descriptors: vec![config],
                is_hub: false,
            });
        } else {
            warn!("UVC device Not Found");
        }

        Ok(devices)
    }

    fn open_device(&mut self, device_id: usize) -> DevResult<Box<dyn UsbDevice>> {
        Ok(Box::new(Dwc2DeviceHandle::new(device_id as u32)))
    }
}

// ============================================================================
// Dwc2DeviceHandle — UsbDevice 实现
// ============================================================================

/// DWC2 设备句柄，封装 sg200x-bsp 的 EP0/Bulk/Isoch 操作。
pub struct Dwc2DeviceHandle {
    device_id: u32,
    ep0_mps: u32,
    /// Isoch 端点每微帧最大包大小（从端点描述符解析）
    isoch_mps: u16,
}

impl Dwc2DeviceHandle {
    pub fn new(device_id: u32) -> Self {
        Self {
            device_id,
            ep0_mps: 64,
            isoch_mps: 3072, // HS default: 3 × 1024
        }
    }

    pub fn with_ep0_mps(mut self, mps: u32) -> Self {
        self.ep0_mps = mps;
        self
    }

    pub fn with_isoch_mps(mut self, mps: u16) -> Self {
        self.isoch_mps = mps;
        self
    }
}

impl UsbDevice for Dwc2DeviceHandle {
    fn descriptor(&self) -> &DeviceDescriptor {
        unimplemented!("descriptor() — provided by USBHost layer")
    }

    fn config_descriptors(&self) -> &[ConfigurationDescriptor] {
        unimplemented!("config_descriptors() — provided by USBHost layer")
    }

    fn set_configuration(&mut self, value: u8) -> DevResult<()> {
        dwc2::ep0::set_configuration(self.device_id, value, self.ep0_mps).map_err(|e| {
            error!("set_configuration({value}) failed: {e:?}");
            DevError::Io
        })
    }

    fn claim_interface(&mut self, interface: u8, alternate: u8) -> DevResult<()> {
        // SET_INTERFACE: 激活指定的 alternate setting
        let setup = SetupPacket::new(
            0x01,       // OUT, Standard, Interface
            0x0B,       // SET_INTERFACE
            alternate as u16, // wValue = alternate setting
            interface as u16, // wIndex = interface number
            0,           // no data stage
        );
        let raw = setup_to_raw(&setup);
        dwc2::ep0::ep0_control_write_no_data(self.device_id, raw, self.ep0_mps).map_err(|e| {
            error!("SET_INTERFACE(iface={interface}, alt={alternate}) failed: {e:?}");
            DevError::Io
        })?;
        debug!("SET_INTERFACE(iface={interface}, alt={alternate}) ok");
        Ok(())
    }

    fn control_in(&mut self, setup: SetupPacket, buf: &mut [u8]) -> DevResult<usize> {
        let raw = setup_to_raw(&setup);
        dwc2::ep0::ep0_control_read(self.device_id, raw, self.ep0_mps, buf).map_err(|e| {
            error!("control_in failed: {e:?}");
            DevError::Io
        })?;
        Ok(buf.len())
    }

    fn control_out(&mut self, setup: SetupPacket, buf: &[u8]) -> DevResult<usize> {
        let raw = setup_to_raw(&setup);
        if buf.is_empty() {
            dwc2::ep0::ep0_control_write_no_data(self.device_id, raw, self.ep0_mps).map_err(
                |e| {
                    error!("control_out (no data) failed: {e:?}");
                    DevError::Io
                },
            )?;
            Ok(0)
        } else {
            dwc2::ep0::ep0_control_write(self.device_id, raw, self.ep0_mps, buf).map_err(|e| {
                error!("control_out failed: {e:?}");
                DevError::Io
            })?;
            Ok(buf.len())
        }
    }

    fn bulk_in(&mut self, ep_addr: u8, buf: &mut [u8]) -> DevResult<usize> {
        let r = dwc2::ep0::bulk_in(
            self.device_id,
            ep_addr as u32,
            512,
            dwc2::ep0::PID_DATA0,
            buf.len(),
            BULK_DMA_OFF,
        )
        .map_err(|e| {
            error!("bulk_in ep=0x{ep_addr:02x} failed: {e:?}");
            DevError::Io
        })?;
        dwc2::ep0::dma_copy_out(BULK_DMA_OFF, &mut buf[..r]);
        Ok(r)
    }

    fn bulk_out(&mut self, ep_addr: u8, buf: &[u8]) -> DevResult<usize> {
        dwc2::ep0::bulk_out(
            self.device_id,
            ep_addr as u32,
            512,
            dwc2::ep0::PID_DATA0,
            buf,
            BULK_DMA_OFF,
        )
        .map_err(|e| {
            error!("bulk_out ep=0x{ep_addr:02x} failed: {e:?}");
            DevError::Io
        })?;
        Ok(buf.len())
    }

    fn isoch_in(&mut self, ep_addr: u8, buf: &mut [u8]) -> DevResult<usize> {
        let jpeg = self.capture_uvc_frame(ep_addr)?;
        let len = jpeg.len().min(buf.len());
        buf[..len].copy_from_slice(&jpeg[..len]);
        Ok(len)
    }
}

// ============================================================================
// Dwc2DeviceHandle — UVC 帧抓取（委托 sg200x-bsp）
// ============================================================================

impl Dwc2DeviceHandle {
    /// 使用 sg200x-bsp 的 `uvc_capture_one_frame` 抓取完整 JPEG 帧。
    ///
    /// 内部处理 isoch 微帧循环、UVC payload header 解析、EOF 检测、
    /// FID toggling 和 DMA 窗口管理，返回纯净的 JPEG 字节。
    pub fn capture_uvc_frame(&self, ep_addr: u8) -> DevResult<alloc::vec::Vec<u8>> {
        use sg200x_bsp::usb::class::uvc::{
            UvcStreamSelection, UvcXferKind, UVC_ASSEMBLED_JPEG_DMA_OFF,
        };

        let sel = UvcStreamSelection {
            vs_interface: 0, // capture 阶段不使用
            alt_setting: 0,
            ep_num: ep_addr & 0x7F,
            mps_raw: self.isoch_mps,
            xfer: UvcXferKind::Isoch,
            format_index: 1,
            frame_index: 1,
            frame_interval: 333333,
            is_mjpeg: true,
            frame_w: 0,
            frame_h: 0,
            negotiated_payload_size: (self.isoch_mps & 0x7ff) as u32,
            negotiated_frame_size: 200 * 1024, // 200KB 上限
            isoch_alts_count: 0,
            isoch_alts: [(0, 0); 8],
        };

        let n = sg200x_bsp::usb::class::uvc::uvc_capture_one_frame(
            self.device_id,
            self.ep0_mps,
            &sel,
        )
        .map_err(|e| {
            error!("uvc_capture_one_frame failed: {e:?}");
            DevError::Io
        })?;

        let data = dwc2::ep0::dma_rx_slice(UVC_ASSEMBLED_JPEG_DMA_OFF, n)
            .ok_or(DevError::Io)?;
        Ok(data.to_vec())
    }
}
