//! USB Mass Storage Class (MSC) 驱动。
//!
//! 基于 Bulk-Only Transport (BOT) 协议，封装 CBW/CSW 和 SCSI 命令。
//! 当前为骨架实现，Phase 4 后完善。

use ax_driver::prelude::*;

use crate::Device;

/// MSC 设备句柄。
pub struct MscDevice {
    _device: Device,
    /// Bulk IN 端点地址
    _bulk_in_ep: u8,
    /// Bulk OUT 端点地址
    _bulk_out_ep: u8,
}

impl MscDevice {
    /// 探测 MSC 设备：解析接口/端点并执行 Bulk-Only Reset。
    pub fn probe(_device: Device) -> DevResult<Self> {
        // TODO: 解析配置描述符找到 MSC 接口和 Bulk IN/OUT 端点
        // TODO: 执行 Bulk-Only Mass Storage Reset (class-specific request)
        // TODO: Get Max LUN
        Err(DevError::Unsupported)
    }
}
