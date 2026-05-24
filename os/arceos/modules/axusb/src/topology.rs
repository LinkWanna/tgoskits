//! USB 作为主机时，需要对 USB 设备树进行扫描和解析，构建设备树拓扑结构（topology）以供上层使用。

pub use sg200x_bsp::usb::host::topology::*;
