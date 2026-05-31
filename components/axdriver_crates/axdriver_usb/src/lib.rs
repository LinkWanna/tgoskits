//! USB 主机协议栈抽象层。
//!
//! 提供 HC-agnostic 的 trait（[`UsbHostController`], [`UsbDevice`], [`UsbEndpoint`]）
//! 和统一的传输抽象（[`TransferRequest`] → [`TransferResult`]）。
//!
//! # 分层
//!
//! ```text
//! axusb (协议栈)  ──trait──►  axdriver_usb (抽象)  ◄──impl──  dwc2 (后端)
//! ```
//!
//! # 类型来源
//!
//! 描述符类型（`DeviceDescriptor`, `ConfigurationDescriptor` 等）来自 `usb-if` crate，
//! 传输类型（`TransferRequest`, `TransferBuffer`, `TransferResult`）也来自 `usb-if`。
//! 本 crate 定义 trait 并提供 DWC2 适配器实现。

#![no_std]
#![cfg_attr(doc, feature(doc_cfg))]

#[macro_use]
extern crate log;
extern crate alloc;

use alloc::{boxed::Box, vec::Vec};

pub mod dwc2;

#[doc(no_inline)]
pub use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
// ── Re-export usb-if 类型 ──
pub use usb_if::descriptor::{
    ConfigurationDescriptor, DeviceDescriptor, EndpointDescriptor, EndpointType,
    InterfaceDescriptor, InterfaceDescriptors,
};
pub use usb_if::{
    DrMode,
    endpoint::{
        EndpointAddress, EndpointInfo, IsoPacketRequest, IsoPacketResult, RequestId,
        TransferBuffer, TransferCompletion, TransferKind, TransferRequest, TransferStatus,
    },
    err::{TransferError, USBError},
    host::{ControlSetup, hub::Speed},
    transfer::{BmRequestType, Direction, Recipient, Request, RequestType},
};

// ── 本地扩展类型 ──

/// USB 标准 Setup Packet（8 字节），用于 EP0 控制传输。
///
/// 这是 usb-if `ControlSetup` 的扁平化替代，方便传统 API。
/// 可通过 `From` trait 互相转换。
#[derive(Debug, Clone, Copy)]
pub struct SetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl SetupPacket {
    pub const fn new(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> Self {
        Self {
            request_type,
            request,
            value,
            index,
            length,
        }
    }

    #[inline]
    pub fn is_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }

    #[inline]
    pub fn is_out(&self) -> bool {
        self.request_type & 0x80 == 0
    }

    /// 转换为 usb-if 的 ControlSetup。
    pub fn to_control_setup(&self) -> ControlSetup {
        ControlSetup {
            request_type: RequestType::Standard, // simplified
            recipient: Recipient::Device,
            request: Request::Other(self.request),
            value: self.value,
            index: self.index,
        }
    }
}

/// 总线枚举后探测到的设备信息。
#[derive(Debug, Clone)]
pub struct ProbedDeviceInfo {
    pub device_id: usize,
    pub descriptor: DeviceDescriptor,
    pub config_descriptors: Vec<ConfigurationDescriptor>,
    pub speed: Speed,
    pub is_hub: bool,
}

// ════════════════════════════════════════════════════════════════════════════
// HC-agnostic Traits
// ════════════════════════════════════════════════════════════════════════════

/// USB 主机控制器 trait。
///
/// 每个 HC 后端（DWC2 / xHCI / ...）提供一个实现了此 trait 的结构体。
pub trait UsbHostController: Send + Sync {
    /// 初始化主机控制器（含平台时钟/PHY/VBUS）。
    fn init(&mut self) -> DevResult<()>;

    /// 探测已连接的 USB 设备（含完整总线枚举：设备描述符 + 配置描述符）。
    fn probe(&mut self) -> DevResult<Vec<ProbedDeviceInfo>>;

    /// 打开一个已探测到的设备，返回设备句柄。
    fn open_device(&mut self, device_id: usize) -> DevResult<Box<dyn UsbDevice>>;
}

/// 已打开的 USB 设备句柄。
///
/// 提供两类 API：
/// 1. **传统 flat 方法**：`control_in/out`、`set_configuration`、`set_interface`
/// 2. **端点模式**：`open_endpoint(ep_addr)` → `UsbEndpoint::submit(TransferRequest)`
///
/// 新 class driver 推荐使用端点模式以获得更清晰的传输语义。
pub trait UsbDevice: Send {
    /// 获取设备描述符。
    fn descriptor(&self) -> &DeviceDescriptor;

    /// 获取配置描述符列表。
    fn config_descriptors(&self) -> &[ConfigurationDescriptor];

    /// 设置活动配置（SET_CONFIGURATION）。
    fn set_configuration(&mut self, value: u8) -> DevResult<()>;

    /// 设置接口 alternate setting（SET_INTERFACE，即 claim interface）。
    fn set_interface(&mut self, interface: u8, alternate: u8) -> DevResult<()>;

    // ── EP0 控制传输（始终可用）──

    /// 控制传输 IN（设备→主机）。
    fn control_in(&mut self, setup: SetupPacket, buf: &mut [u8]) -> DevResult<usize>;

    /// 控制传输 OUT（主机→设备，无数据阶段时 buf 为空）。
    fn control_out(&mut self, setup: SetupPacket, buf: &[u8]) -> DevResult<usize>;

    // ── 端点模式（推荐）──

    /// 按地址打开一个非 EP0 端点。
    ///
    /// 返回的 `UsbEndpoint` 可通过 `submit(TransferRequest)` 执行
    /// Bulk/Interrupt/Isochronous 传输。
    fn open_endpoint(&mut self, ep_addr: u8) -> DevResult<Box<dyn UsbEndpoint>>;

    // ── 便捷方法（默认实现，委托给 open_endpoint + submit）──

    /// 批量传输 IN。
    fn bulk_in(&mut self, ep_addr: u8, buf: &mut [u8]) -> DevResult<usize> {
        let mut ep = self.open_endpoint(ep_addr)?;
        let req = TransferRequest::bulk_in(buf);
        let result = ep.submit(req)?;
        Ok(result.actual_length)
    }

    /// 批量传输 OUT。
    fn bulk_out(&mut self, ep_addr: u8, buf: &[u8]) -> DevResult<usize> {
        let mut ep = self.open_endpoint(ep_addr)?;
        let req = TransferRequest::bulk_out(buf);
        let result = ep.submit(req)?;
        Ok(result.actual_length)
    }

    /// 同步传输 IN（单微帧）。
    fn isoch_in(&mut self, ep_addr: u8, buf: &mut [u8]) -> DevResult<usize> {
        let mut ep = self.open_endpoint(ep_addr)?;
        let req = TransferRequest::iso_in(buf, &[buf.len()]);
        let result = ep.submit(req)?;
        Ok(result.actual_length)
    }
}

/// USB 端点句柄。
///
/// 通过 [`UsbDevice::open_endpoint`] 获取。
/// 每次 `submit()` 执行一次完整的 USB 传输（可包含 NAK 重试）。
pub trait UsbEndpoint: Send {
    /// 端点信息（地址、传输类型、MPS、间隔）。
    fn info(&self) -> EndpointInfo;

    /// 提交一次传输请求。
    ///
    /// `TransferRequest` 的类型自身编码了传输语义：
    /// - `Control` → EP0 控制传输（含 SETUP + 可选 DATA + STATUS）
    /// - `Bulk` → 批量传输
    /// - `Interrupt` → 中断传输
    /// - `Isochronous` → 同步传输（单微帧）
    fn submit(&mut self, request: TransferRequest) -> DevResult<TransferCompletion>;
}
