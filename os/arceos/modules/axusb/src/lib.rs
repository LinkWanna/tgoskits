//! ArceOS USB 主机协议栈。
//!
//! 基于 axdriver_usb 的 HC-agnostic trait，提供：
//! - [`USBHost`]：主机控制器管理 + 设备枚举
//! - [`Device`]：已打开设备的句柄封装
//! - [`class::uvc`]：USB Video Class driver
//!
//! # 使用示例
//!
//! ```ignore
//! // 1. 创建并初始化 HC
//! let mut hc = Dwc2HostController::new(base_addr);
//! hc.init()?;
//!
//! // 2. 创建 USBHost 并枚举设备
//! let mut host = USBHost::new(hc);
//! host.enumerate()?;
//!
//! // 3. 打开 UVC 设备
//! let dev = host.open(0)?;
//!
//! // 4. 使用 class driver
//! let mut cam = UvcCamera::probe(dev)?;
//! let frame = cam.capture_frame()?;
//! ```

#![no_std]

#[macro_use]
extern crate log;
extern crate alloc;

use alloc::{boxed::Box, vec::Vec};

use ax_driver::{AxDeviceContainer, prelude::*};
use ax_driver_usb::{
    ConfigurationDescriptor, DeviceDescriptor, EndpointInfo, ProbedDeviceInfo, SetupPacket,
    UsbDevice as UsbDeviceTrait, UsbEndpoint, UsbHostController,
};
use ax_lazyinit::LazyInit;
use ax_sync::Mutex;

pub mod class;
pub mod imgcat;

// ============================================================================
// USBHost — 主机控制器 + 设备管理
// ============================================================================

/// USB 主机控制器封装。
///
/// 持有 HC 后端 + 枚举到的设备信息列表，提供设备打开和管理功能。
pub struct USBHost {
    hc: Box<dyn UsbHostController>,
    devices: Vec<ProbedDeviceInfo>,
}

impl USBHost {
    /// 从已初始化的 HC 后端创建 USBHost。
    ///
    /// HC 必须先调用 `init()` 完成平台和控制器初始化。
    /// 创建后调用 [`enumerate`] 进行总线枚举。
    pub fn new(hc: impl UsbHostController + 'static) -> Self {
        Self {
            hc: Box::new(hc),
            devices: Vec::new(),
        }
    }

    /// 执行总线枚举，发现已连接的设备。
    pub fn enumerate(&mut self) -> DevResult<()> {
        self.devices = self.hc.probe()?;
        info!("USB: {} device(s) found on bus", self.devices.len());
        for d in &self.devices {
            info!(
                "USB: dev#{} VID={:04x} PID={:04x} class={:02x} {}",
                d.device_id,
                d.descriptor.vendor_id,
                d.descriptor.product_id,
                d.descriptor.class,
                if d.is_hub { "[Hub]" } else { "" }
            );
        }
        Ok(())
    }

    /// 获取枚举到的设备信息列表。
    pub fn device_list(&self) -> &[ProbedDeviceInfo] {
        &self.devices
    }

    /// 打开指定设备，返回高层 Device 句柄。
    pub fn open(&mut self, device_id: usize) -> DevResult<Device> {
        let handle = self.hc.open_device(device_id)?;
        // 从 probe 阶段的 device_info 获取描述符
        let info = self
            .devices
            .iter()
            .find(|d| d.device_id == device_id)
            .ok_or(DevError::Unsupported)?;
        Ok(Device {
            handle,
            descriptor: info.descriptor.clone(),
            configs: info.config_descriptors.clone(),
        })
    }

    /// 查找第一个匹配 class 的设备。
    pub fn find_device_by_class(&self, class: u8) -> Option<&ProbedDeviceInfo> {
        self.devices.iter().find(|d| d.descriptor.class == class)
    }

    /// 查找第一个匹配 VID/PID 的设备。
    pub fn find_device_by_vid_pid(&self, vid: u16, pid: u16) -> Option<&ProbedDeviceInfo> {
        self.devices
            .iter()
            .find(|d| d.descriptor.vendor_id == vid && d.descriptor.product_id == pid)
    }
}

// ============================================================================
// Device — 已打开设备的高层封装
// ============================================================================

/// 已打开的 USB 设备。
///
/// 通过 [`USBHost::open`] 获得，封装底层 `UsbDevice` trait 对象 +
/// probe 阶段缓存的描述符信息。
pub struct Device {
    handle: Box<dyn UsbDeviceTrait>,
    descriptor: DeviceDescriptor,
    configs: Vec<ConfigurationDescriptor>,
}

impl Device {
    /// 获取设备描述符。
    pub fn descriptor(&self) -> &DeviceDescriptor {
        &self.descriptor
    }

    /// 获取配置描述符列表。
    pub fn config_descriptors(&self) -> &[ConfigurationDescriptor] {
        &self.configs
    }

    /// 获取底层设备句柄引用。
    pub fn handle(&self) -> &dyn UsbDeviceTrait {
        self.handle.as_ref()
    }

    /// 获取底层设备句柄可变引用。
    pub fn handle_mut(&mut self) -> &mut dyn UsbDeviceTrait {
        self.handle.as_mut()
    }

    /// 控制传输 IN。
    pub fn control_in(&mut self, setup: SetupPacket, buf: &mut [u8]) -> DevResult<usize> {
        self.handle.control_in(setup, buf)
    }

    /// 控制传输 OUT。
    pub fn control_out(&mut self, setup: SetupPacket, buf: &[u8]) -> DevResult<usize> {
        self.handle.control_out(setup, buf)
    }

    /// 设置活动配置。
    pub fn set_configuration(&mut self, value: u8) -> DevResult<()> {
        self.handle.set_configuration(value)
    }

    /// 声明接口（SET_INTERFACE）。
    pub fn claim_interface(&mut self, interface: u8, alternate: u8) -> DevResult<()> {
        self.handle.set_interface(interface, alternate)
    }

    /// 按地址 + 端点信息打开端点。
    pub fn open_endpoint_with(
        &mut self,
        ep_addr: u8,
        info: EndpointInfo,
    ) -> DevResult<Box<dyn UsbEndpoint>> {
        self.handle.open_endpoint_with(ep_addr, info)
    }

    /// 批量传输 IN。
    pub fn bulk_in(&mut self, ep_addr: u8, buf: &mut [u8]) -> DevResult<usize> {
        self.handle.bulk_in(ep_addr, buf)
    }

    /// 批量传输 OUT。
    pub fn bulk_out(&mut self, ep_addr: u8, buf: &[u8]) -> DevResult<usize> {
        self.handle.bulk_out(ep_addr, buf)
    }

    /// 同步传输 IN。
    pub fn isoch_in(&mut self, ep_addr: u8, buf: &mut [u8]) -> DevResult<usize> {
        self.handle.isoch_in(ep_addr, buf)
    }
}

// ============================================================================
// 全局实例 & 初始化入口
// ============================================================================

static USB_HOST: LazyInit<Mutex<USBHost>> = LazyInit::new();

/// 初始化 USB 子系统。
///
/// 由 axruntime 在设备初始化阶段调用。从平台设备树提取 DWC2 基址，
/// 初始化主机控制器，执行总线枚举。
pub fn init_usb(mut usb_devs: AxDeviceContainer<AxUsbDevice>) {
    info!("Initialize USB subsystem...");

    // 从旧版 AxUsbDevice（Dwc2 结构）提取 DWC2 MMIO 基址
    if let Some(dev) = usb_devs.take_one() {
        let dwc2_base = dev.base();
        info!("USB: DWC2 base = {:#010x}", dwc2_base);

        // 使用新版 Dwc2HostController（封装平台初始化 + HC 初始化）
        let mut hc = ax_driver_usb::dwc2::Dwc2HostController::new(dwc2_base);
        match hc.init() {
            Ok(()) => {
                let mut host = USBHost::new(hc);
                match host.enumerate() {
                    Ok(()) => {
                        info!(
                            "USB: initialization complete, {} device(s) on bus",
                            host.device_list().len()
                        );
                        USB_HOST.init_once(Mutex::new(host));
                    }
                    Err(e) => {
                        warn!("USB: enumeration failed: {:?}", e);
                        // 即使枚举失败也保存 host（可能没有设备连接）
                        USB_HOST.init_once(Mutex::new(host));
                    }
                }
            }
            Err(e) => {
                error!("USB: HC init failed: {:?}", e);
            }
        }
    } else {
        warn!("USB: No USB device found (check platform config)");
    }
}

/// 是否有 USB 主机控制器可用。
pub fn has_usb() -> bool {
    USB_HOST.is_inited()
}

/// 获取全局 USB 主机实例。
pub fn usb_host() -> &'static Mutex<USBHost> {
    USB_HOST
        .get()
        .expect("USB host not initialized (call init_usb first)")
}
