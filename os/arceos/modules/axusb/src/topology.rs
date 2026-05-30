//! USB 总线设备信息。
//!
//! Re-export axdriver_usb 的探测设备信息类型，供上层使用。

pub use ax_driver_usb::{ProbedDeviceInfo, device::DeviceDescriptor};
