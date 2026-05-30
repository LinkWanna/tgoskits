//! USB 主机协议栈抽象层。
//!
//! 提供 HC-agnostic 的 trait（[`UsbHostController`], [`UsbDevice`], [`UsbEndpoint`]）
//! 和 USB 标准描述符类型（[`crate::device`]）。
//!
//! # 分层
//!
//! ```text
//! axusb (协议栈)  ──trait──►  axdriver_usb (抽象)  ◄──impl──  dwc2 (后端)
//! ```

#![no_std]
#![cfg_attr(doc, feature(doc_cfg))]

#[macro_use]
extern crate log;
extern crate alloc;

use alloc::{boxed::Box, vec::Vec};

pub mod device;
pub mod dwc2;

#[doc(no_inline)]
pub use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};

use crate::device::{ConfigurationDescriptor, DeviceDescriptor};

// ============================================================================
// USB 标准传输抽象
// ============================================================================

/// USB 标准 Setup Packet（8 字节），用于控制传输的 SETUP 阶段。
#[derive(Debug, Clone, Copy)]
pub struct SetupPacket {
    /// bmRequestType — bit 7: 方向, bit 5-6: 类型, bit 0-4: 接收者
    pub request_type: u8,
    /// bRequest — 具体请求码（如 GET_DESCRIPTOR=6, SET_ADDRESS=5）
    pub request: u8,
    /// wValue — 请求相关值（little-endian）
    pub value: u16,
    /// wIndex — 请求相关索引（little-endian）
    pub index: u16,
    /// wLength — 数据阶段长度（little-endian）
    pub length: u16,
}

impl SetupPacket {
    /// 构造一个新的 Setup Packet。
    pub const fn new(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> Self {
        Self {
            request_type,
            request,
            value,
            index,
            length,
        }
    }

    /// 方向：bit 7 = 1 为 IN（设备→主机）
    #[inline]
    pub fn is_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }

    /// 方向：bit 7 = 0 为 OUT（主机→设备）
    #[inline]
    pub fn is_out(&self) -> bool {
        self.request_type & 0x80 == 0
    }
}

/// USB 数据传输方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out = 0,
    In  = 1,
}

/// USB 端点传输类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferType {
    Control     = 0,
    Isochronous = 1,
    Bulk        = 2,
    Interrupt   = 3,
}

/// 总线枚举后探测到的设备信息。
#[derive(Debug, Clone)]
pub struct ProbedDeviceInfo {
    /// 设备 ID（由 HC 分配，用于 open_device）
    pub device_id: usize,
    /// 设备描述符
    pub descriptor: DeviceDescriptor,
    /// 配置描述符列表
    pub config_descriptors: Vec<ConfigurationDescriptor>,
    /// 是否为 Hub
    pub is_hub: bool,
}

// ============================================================================
// HC-agnostic Trait
// ============================================================================

/// USB 主机控制器 trait。
///
/// 每个 HC 后端（DWC2 / xHCI / ...）提供一个实现了此 trait 的结构体。
pub trait UsbHostController: Send + Sync {
    /// 初始化主机控制器（含平台时钟/PHY/VBUS）。
    fn init(&mut self) -> DevResult<()>;

    /// 探测已连接的 USB 设备（含总线枚举）。
    fn probe(&mut self) -> DevResult<Vec<ProbedDeviceInfo>>;

    /// 打开一个已探测到的设备，返回设备句柄。
    fn open_device(&mut self, device_id: usize) -> DevResult<Box<dyn UsbDevice>>;
}

/// 已打开的 USB 设备句柄。
pub trait UsbDevice: Send {
    /// 获取设备描述符。
    fn descriptor(&self) -> &DeviceDescriptor;

    /// 获取配置描述符列表。
    fn config_descriptors(&self) -> &[ConfigurationDescriptor];

    /// 设置活动配置（SET_CONFIGURATION）。
    fn set_configuration(&mut self, value: u8) -> DevResult<()>;

    /// 声明（claim）一个接口。
    fn claim_interface(&mut self, interface: u8, alternate: u8) -> DevResult<()>;

    /// 控制传输 IN（设备→主机）。
    fn control_in(&mut self, setup: SetupPacket, buf: &mut [u8]) -> DevResult<usize>;

    /// 控制传输 OUT（主机→设备）。
    fn control_out(&mut self, setup: SetupPacket, buf: &[u8]) -> DevResult<usize>;

    /// 批量传输 IN。
    fn bulk_in(&mut self, ep_addr: u8, buf: &mut [u8]) -> DevResult<usize>;

    /// 批量传输 OUT。
    fn bulk_out(&mut self, ep_addr: u8, buf: &[u8]) -> DevResult<usize>;

    /// 同步传输 IN（常用于 UVC 视频流）。
    fn isoch_in(&mut self, ep_addr: u8, buf: &mut [u8]) -> DevResult<usize>;
}

/// USB 端点句柄。
pub trait UsbEndpoint {
    /// 端点地址（bit 7 为方向）。
    fn address(&self) -> u8;

    /// 端点传输类型。
    fn transfer_type(&self) -> TransferType;

    /// 最大包大小。
    fn max_packet_size(&self) -> u16;

    /// IN 传输（读数据）。
    fn transfer_in(&mut self, buf: &mut [u8]) -> DevResult<usize>;

    /// OUT 传输（写数据）。
    fn transfer_out(&mut self, buf: &[u8]) -> DevResult<usize>;
}
