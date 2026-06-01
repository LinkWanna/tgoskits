//! DWC2 USB 主机控制器适配器。
//!
//! 封装 `dwc2_driver::Dwc2Controller`，实现 axdriver_usb 的 HC-agnostic trait：
//! - [`UsbHostController`]：控制器初始化 + 设备枚举
//! - [`UsbDevice`]：设备句柄（EP0 控制 + 端点打开）
//! - [`UsbEndpoint`]：端点传输（`submit(TransferRequest)`）
//!
//! 同时包含 SG2002 平台初始化（PHY + pinctrl + VBUS）。

pub mod osal_impl;
pub mod phy;
pub mod pinctrl;

use alloc::{boxed::Box, vec::Vec};

use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use dwc2_driver::{Dwc2Controller, Speed, channel::EpType};
use spin::Mutex;

use crate::{
    ConfigurationDescriptor, DeviceDescriptor, EndpointInfo, ProbedDeviceInfo, RequestId,
    SetupPacket, TransferBuffer, TransferCompletion, TransferRequest, TransferStatus, UsbDevice,
    UsbEndpoint, UsbHostController,
};

// ════════════════════════════════════════════════════════════════════════════
// 平台初始化
// ════════════════════════════════════════════════════════════════════════════

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
    // 等待 VBUS 稳定约 2 秒
    for _ in 0..2_000_000u32 {
        core::hint::spin_loop();
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 辅助
// ════════════════════════════════════════════════════════════════════════════

/// 将 crate::SetupPacket 转为 `[u8; 8]` SETUP 包。
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

/// 将 usb-if TransferBuffer 转为 `&[u8]` 或 `&mut [u8]`。
unsafe fn buffer_slice(buf: TransferBuffer) -> &'static mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(buf.ptr.as_ptr(), buf.len) }
}

// ════════════════════════════════════════════════════════════════════════════
// Dwc2 — 设备描述符持有者（供 axdriver 设备注册使用）
// ════════════════════════════════════════════════════════════════════════════

pub struct Dwc2 {
    dwc2_base: usize,
}

impl Dwc2 {
    pub fn new(dwc2_base: usize) -> Self {
        Self { dwc2_base }
    }

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

// ════════════════════════════════════════════════════════════════════════════
// Dwc2HostController — UsbHostController 实现
// ════════════════════════════════════════════════════════════════════════════

/// DWC2 主机控制器适配器。
///
/// 持有共享的 `Dwc2Controller`（通过 `&'static Mutex` 在 device/endpoint 间共享）。
pub struct Dwc2HostController {
    dwc2_base: usize,
    controller: Option<&'static Mutex<Dwc2Controller>>,
}

impl Dwc2HostController {
    pub fn new(dwc2_base: usize) -> Self {
        Self {
            dwc2_base,
            controller: None,
        }
    }

    fn ctrl(&self) -> &'static Mutex<Dwc2Controller> {
        self.controller
            .expect("DWC2 controller not initialized (call init first)")
    }
}

impl UsbHostController for Dwc2HostController {
    fn init(&mut self) -> DevResult<()> {
        unsafe { platform_init() };

        // 设置 sg200x-bsp 全局状态（enumerate_topology_only 依赖）
        sg200x_bsp::usb::platform::set_dwc2_base_virt(self.dwc2_base);
        sg200x_bsp::usb::platform::set_usb_dma_to_phys_fn(Some(|p| {
            ax_hal::mem::virt_to_phys(ax_hal::mem::VirtAddr::from(p as usize)).as_usize() as u32
        }));
        sg200x_bsp::usb::log::set_usb_log_fn(|s| info!("{s}"));

        let osal: &'static osal_impl::AxOsal = Box::leak(Box::new(osal_impl::AxOsal::new()));
        let mut ctrl = unsafe { dwc2_driver::Dwc2Controller::new(self.dwc2_base, osal) };
        ctrl.hw_init().map_err(|e| {
            error!("DWC2 hw_init failed: {e}");
            DevError::Io
        })?;

        // 分配 DMA 缓冲区（384KB — 与 sg200x-bsp 一致，足够 UVC 帧）
        ctrl.alloc_dma_buf(384 * 1024).map_err(|e| {
            error!("DWC2 DMA alloc failed: {e}");
            DevError::NoMemory
        })?;

        // 打印 DMA 缓冲区信息用于调试
        if let Some(dma) = ctrl.dma_buf() {
            info!(
                "DWC2 DMA buf: VA={:#018x} PA={:#010x} size={}",
                dma.va_ptr() as usize,
                dma.phys(),
                dma.size()
            );
        }

        // Leak 为 &'static，供 device/endpoint 共享
        let ctrl: &'static Mutex<Dwc2Controller> = Box::leak(Box::new(Mutex::new(ctrl)));
        self.controller = Some(ctrl);

        info!("DWC2 host controller initialized");
        Ok(())
    }

    fn probe(&mut self) -> DevResult<Vec<ProbedDeviceInfo>> {
        use sg200x_bsp::usb::host;

        let mut last_err = None;
        let extras = (0..4)
            .find_map(|attempt| {
                if attempt > 0 {
                    for _ in 0..1_500_000u32 * attempt as u32 {
                        core::hint::spin_loop();
                    }
                }
                match host::enumerate_topology_only() {
                    Ok(ex) => Some(ex),
                    Err(e) => {
                        warn!("USB: enumerate attempt #{} failed: {:?}", attempt + 1, e);
                        last_err = Some(e);
                        None
                    }
                }
            })
            .ok_or_else(|| {
                warn!("USB: all enumeration retries failed");
                DevError::Unsupported
            })?;

        let mut devices = Vec::new();
        if let Some(uvc) = extras.uvc {
            let desc_buf = {
                let mut buf = [0u8; 18];
                // Read device descriptor via sg200x-bsp EP0
                use sg200x_bsp::usb::{host::dwc2::ep0 as dwc2_ep0, setup};
                let _ = dwc2_ep0::ep0_control_read(
                    uvc.addr as u32,
                    setup::get_descriptor_device(18),
                    uvc.ep0_mps,
                    &mut buf,
                );
                buf
            };
            let descriptor = DeviceDescriptor::parse(&desc_buf).unwrap_or(DeviceDescriptor {
                usb_version: 0x0200,
                class: 0xEF,
                subclass: 0x02,
                protocol: 0x01,
                max_packet_size_0: uvc.ep0_mps as u8,
                vendor_id: uvc.vid,
                product_id: uvc.pid,
                device_version: 0,
                manufacturer_string_index: None,
                product_string_index: None,
                serial_number_string_index: None,
                num_configurations: 1,
            });

            devices.push(ProbedDeviceInfo {
                device_id: uvc.addr as usize,
                descriptor,
                config_descriptors: alloc::vec![],
                speed: Speed::High,
                is_hub: false,
            });
        }

        info!("USB: {} device(s) found on bus", devices.len());
        Ok(devices)
    }

    fn open_device(&mut self, device_id: usize) -> DevResult<Box<dyn UsbDevice>> {
        Ok(Box::new(Dwc2DeviceAdapter {
            dev_addr: device_id as u8,
            ep0_mps: 64, // 从 probe 阶段获取的实际值应由上层传入
            speed: Speed::High,
            controller: self.ctrl(),
        }))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Dwc2DeviceAdapter — UsbDevice 实现
// ════════════════════════════════════════════════════════════════════════════

/// DWC2 设备适配器。
pub struct Dwc2DeviceAdapter {
    dev_addr: u8,
    ep0_mps: u16,
    speed: Speed,
    controller: &'static Mutex<Dwc2Controller>,
}

impl Dwc2DeviceAdapter {
    /// 从 ProbedDeviceInfo 填充更多字段。
    pub fn with_info(mut self, info: &ProbedDeviceInfo) -> Self {
        self.ep0_mps = info.descriptor.max_packet_size_0 as u16;
        self.speed = info.speed;
        self
    }
}

impl UsbDevice for Dwc2DeviceAdapter {
    fn descriptor(&self) -> &DeviceDescriptor {
        // 描述符由上层 USBHost 缓存，此处通过 trait 返回不可行
        // 实际使用中 axusb::Device 会缓存描述符
        unimplemented!("descriptor() — provided by USBHost layer")
    }

    fn config_descriptors(&self) -> &[ConfigurationDescriptor] {
        unimplemented!("config_descriptors() — provided by USBHost layer")
    }

    fn set_configuration(&mut self, value: u8) -> DevResult<()> {
        sg200x_bsp::usb::host::dwc2::ep0::set_configuration(
            self.dev_addr as u32,
            value,
            self.ep0_mps as u32,
        )
        .map_err(|e| {
            error!("SET_CONFIGURATION({value}) failed: {e:?}");
            DevError::Io
        })
    }

    fn set_interface(&mut self, interface: u8, alternate: u8) -> DevResult<()> {
        let setup = SetupPacket::new(0x01, 0x0B, alternate as u16, interface as u16, 0);
        let raw = setup_to_raw(&setup);
        let mut ctrl = self.controller.lock();
        ctrl.ep0_control_no_data(self.dev_addr, &raw, self.ep0_mps)
            .map_err(|e| {
                error!("SET_INTERFACE(if={interface}, alt={alternate}) failed: {e}");
                DevError::Io
            })
    }

    fn control_in(&mut self, setup: SetupPacket, buf: &mut [u8]) -> DevResult<usize> {
        let raw = setup_to_raw(&setup);
        let mut ctrl = self.controller.lock();
        ctrl.ep0_control_in(self.dev_addr, &raw, buf, self.ep0_mps)
            .map_err(|e| {
                error!("control_in failed: {e}");
                DevError::Io
            })?;
        Ok(buf.len())
    }

    fn control_out(&mut self, setup: SetupPacket, buf: &[u8]) -> DevResult<usize> {
        let raw = setup_to_raw(&setup);
        if buf.is_empty() {
            sg200x_bsp::usb::host::dwc2::ep0::ep0_control_write_no_data(
                self.dev_addr as u32,
                raw,
                self.ep0_mps as u32,
            )
            .map_err(|e| {
                error!("control_out (no data) failed: {e:?}");
                DevError::Io
            })?;
            Ok(0)
        } else {
            sg200x_bsp::usb::host::dwc2::ep0::ep0_control_write(
                self.dev_addr as u32,
                raw,
                self.ep0_mps as u32,
                buf,
            )
            .map_err(|e| {
                error!("control_out failed: {e:?}");
                DevError::Io
            })?;
            Ok(buf.len())
        }
    }

    fn open_endpoint(&mut self, info: EndpointInfo) -> DevResult<Box<dyn UsbEndpoint>> {
        let ep_addr = info.address.raw();
        let ep_type = match info.transfer_type {
            crate::EndpointType::Control => EpType::Control,
            crate::EndpointType::Isochronous => EpType::Isochronous,
            crate::EndpointType::Bulk => EpType::Bulk,
            crate::EndpointType::Interrupt => EpType::Interrupt,
        };
        Ok(Box::new(Dwc2EndpointAdapter {
            ep_addr,
            ep_type,
            dev_addr: self.dev_addr,
            controller: self.controller,
            _mps: info.max_packet_size,
        }))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Dwc2EndpointAdapter — UsbEndpoint 实现
// ════════════════════════════════════════════════════════════════════════════

/// DWC2 端点适配器。
pub struct Dwc2EndpointAdapter {
    ep_addr: u8,
    ep_type: EpType,
    dev_addr: u8,
    controller: &'static Mutex<Dwc2Controller>,
    _mps: u16,
}

impl Dwc2EndpointAdapter {
    /// 用端点描述符更新类型和 MPS。
    pub fn with_info(mut self, info: &EndpointInfo) -> Self {
        self.ep_type = match info.transfer_type {
            crate::EndpointType::Control => EpType::Control,
            crate::EndpointType::Isochronous => EpType::Isochronous,
            crate::EndpointType::Bulk => EpType::Bulk,
            crate::EndpointType::Interrupt => EpType::Interrupt,
        };
        self._mps = info.max_packet_size;
        self
    }
}

impl UsbEndpoint for Dwc2EndpointAdapter {
    fn info(&self) -> EndpointInfo {
        EndpointInfo {
            address: self.ep_addr.into(),
            transfer_type: match self.ep_type {
                EpType::Control => crate::EndpointType::Control,
                EpType::Isochronous => crate::EndpointType::Isochronous,
                EpType::Bulk => crate::EndpointType::Bulk,
                EpType::Interrupt => crate::EndpointType::Interrupt,
            },
            direction: if self.ep_addr & 0x80 != 0 {
                crate::Direction::In
            } else {
                crate::Direction::Out
            },
            max_packet_size: self._mps,
            packets_per_microframe: 1,
            interval: 0,
        }
    }

    fn submit(&mut self, request: TransferRequest) -> DevResult<TransferCompletion> {
        let mut ctrl = self.controller.lock();

        let ep_num = self.ep_addr & 0x7F;
        let ep_dir_in = self.ep_addr & 0x80 != 0;

        match request {
            TransferRequest::Bulk {
                direction,
                buffer: Some(buf),
            } => {
                let dma = ctrl.dma_buf().ok_or(DevError::NoMemory)?;
                let dma_phys = dma.phys_at(0);
                let dma_size = dma.size();
                let dma_va = dma.va_ptr();

                if direction == crate::Direction::In {
                    let data = unsafe { buffer_slice(buf) };
                    let xfer = data.len().min(dma_size);

                    let mut ch = ctrl
                        .alloc_channel(self.dev_addr, ep_num, true, self.ep_type, self._mps)
                        .map_err(|_| DevError::Io)?;

                    // 使用通道直接传输（绕过 Dwc2Endpoint）
                    let n = ch
                        .execute_with_retry(
                            dwc2_driver::channel::PID_DATA0,
                            xfer as u32,
                            1,
                            dma_phys,
                        )
                        .map_err(|_| DevError::Io)?;

                    // DMA 写入后必须 invalidate cache（匹配 BSP dcache_invalidate_after_dma）
                    ctrl.dma_cache_invalidate(dma_va, n);

                    // 从 DMA 缓冲区拷贝结果
                    unsafe {
                        let s = core::slice::from_raw_parts(ctrl.dma_buf().unwrap().va_ptr(), n);
                        let len = s.len().min(data.len());
                        data[..len].copy_from_slice(&s[..len]);
                    }

                    ctrl.release_channel(ch);
                    Ok(TransferCompletion {
                        request_id: RequestId::new(0),
                        status: TransferStatus::Completed,
                        actual_length: n,
                        iso_packets: alloc::vec![],
                    })
                } else {
                    let data = unsafe { buffer_slice(buf) };
                    let xfer = data.len().min(dma_size);

                    let mut ch = ctrl
                        .alloc_channel(self.dev_addr, ep_num, false, self.ep_type, self._mps)
                        .map_err(|_| DevError::Io)?;

                    // 拷贝数据到 DMA 缓冲区
                    unsafe {
                        let s =
                            core::slice::from_raw_parts_mut(ctrl.dma_buf().unwrap().va_ptr(), xfer);
                        s[..xfer].copy_from_slice(data);
                    }

                    // DMA 读取前必须 clean cache（匹配 BSP dcache_clean_for_dma）
                    ctrl.dma_cache_clean(dma_va, xfer);

                    let n = ch
                        .execute_with_retry(
                            dwc2_driver::channel::PID_DATA0,
                            xfer as u32,
                            1,
                            dma_phys,
                        )
                        .map_err(|_| DevError::Io)?;

                    ctrl.release_channel(ch);
                    Ok(TransferCompletion {
                        request_id: RequestId::new(0),
                        status: TransferStatus::Completed,
                        actual_length: n,
                        iso_packets: alloc::vec![],
                    })
                }
            }
            TransferRequest::Isochronous {
                direction: _,
                buffer: Some(buf),
                ..
            } => {
                let dma = ctrl.dma_buf().ok_or(DevError::NoMemory)?;
                let dma_phys = dma.phys_at(0);
                let dma_va = dma.va_ptr();
                let data = unsafe { buffer_slice(buf) };

                // 解码 ISOC wMaxPacketSize（匹配 sg200x-bsp isoch_in_uframe）
                let tx_bytes = (self._mps & 0x7FF) as usize;
                let mult = ((self._mps >> 11) & 0x3) as u32 + 1;
                if tx_bytes == 0 || mult > 3 {
                    return Err(DevError::Io);
                }
                let max_uframe = tx_bytes * (mult as usize);
                let xfer = data.len().min(max_uframe).min(dma.size());

                // 通道 MPS 用事务级值（不含 mult 编码）
                let mut ch = ctrl
                    .alloc_channel(
                        self.dev_addr,
                        ep_num,
                        true,
                        EpType::Isochronous,
                        tx_bytes as u16,
                    )
                    .map_err(|_| DevError::Io)?;

                // PID & PKTCNT 匹配 mult
                let (pid, pktcnt) = match mult {
                    3 => (dwc2_driver::channel::PID_DATA2, 3u32),
                    2 => (dwc2_driver::channel::PID_DATA1, 2u32),
                    _ => (dwc2_driver::channel::PID_DATA0, 1u32),
                };

                let n = match ch.execute(pid, xfer as u32, pktcnt, dma_phys) {
                    Ok(n) => {
                        // DMA 写入后必须 invalidate cache（匹配 BSP dcache_invalidate_after_dma）
                        ctrl.dma_cache_invalidate(dma_va, n);
                        unsafe {
                            let s =
                                core::slice::from_raw_parts(ctrl.dma_buf().unwrap().va_ptr(), n);
                            let len = s.len().min(data.len());
                            data[..len].copy_from_slice(&s[..len]);
                        }
                        n
                    }
                    Err(_) => 0, // isoch NAK/error = no data this microframe
                };

                ctrl.release_channel(ch);
                Ok(TransferCompletion {
                    request_id: RequestId::new(0),
                    status: TransferStatus::Completed,
                    actual_length: n,
                    iso_packets: alloc::vec![],
                })
            }
            TransferRequest::Bulk { buffer: None, .. }
            | TransferRequest::Isochronous { buffer: None, .. } => {
                // 无 buffer = ZLP (Zero-Length Packet)
                let mut ch = ctrl
                    .alloc_channel(self.dev_addr, ep_num, ep_dir_in, self.ep_type, self._mps)
                    .map_err(|_| DevError::Io)?;

                let dma = ctrl.dma_buf().ok_or(DevError::NoMemory)?;
                let n = ch
                    .execute_with_retry(dwc2_driver::channel::PID_DATA0, 0, 1, dma.phys_at(0))
                    .map_err(|_| DevError::Io)?;

                ctrl.release_channel(ch);
                Ok(TransferCompletion {
                    request_id: RequestId::new(0),
                    status: TransferStatus::Completed,
                    actual_length: n,
                    iso_packets: alloc::vec![],
                })
            }
            _ => Err(DevError::Unsupported),
        }
    }
}
