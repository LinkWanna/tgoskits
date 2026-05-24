//! Common traits and types for USB device drivers.

#![no_std]
#![cfg_attr(doc, feature(doc_cfg))]

#[macro_use]
extern crate log;
extern crate alloc;

pub mod dwc2;

#[doc(no_inline)]
pub use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use sg200x_bsp::usb::host::TopologyScanExtras;

/// 参考了 CrabUSB 的抽象
pub trait UsbDriverOps: BaseDriverOps {
    /// 初始化 USB 驱动
    fn init(&mut self) -> DevResult<()>;

    /// 探测已连接的设备
    fn device_list(&mut self) -> DevResult<TopologyScanExtras>;
}
