//! DWC2 设备描述符持有者。
//!
//! 在枚举阶段创建，持有设备地址、EP0 MPS 和速度信息。

use crate::speed::Speed;

/// 枚举后的 DWC2 设备信息。
///
/// 不持有通道 — 传输通过 [`Dwc2Controller`] 的 EP0 方法或显式分配的 [`HostChannel`] 进行。
pub struct Dwc2Device {
    /// 设备地址（SET_ADDRESS 分配）
    pub dev_addr: u8,
    /// EP0 最大包大小
    pub ep0_mps: u16,
    /// 设备速度
    pub speed: Speed,
}

impl Dwc2Device {
    pub fn new(dev_addr: u8, ep0_mps: u16, speed: Speed) -> Self {
        Self {
            dev_addr,
            ep0_mps,
            speed,
        }
    }
}
