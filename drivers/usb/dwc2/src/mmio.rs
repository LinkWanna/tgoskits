//! DWC2 MMIO 视图 — 实例化（非全局）寄存器访问。
//!
//! 与 sg200x-bsp 的全局 `AtomicUsize` 方案不同，`Dwc2Mmio` 是实例化的：
//! 每个控制器实例持有自己的基址，多个控制器可以共存。

use crate::reg::{Dwc2HostChannel, Dwc2Regs};

/// DWC2 寄存器块的实例化 MMIO 视图。
///
/// 通过 `unsafe` 构造方法接收已验证的虚拟基址。
/// 控制器生命周期 = 程序生命周期（`&'static self`）。
pub struct Dwc2Mmio {
    base: usize,
}

impl Dwc2Mmio {
    /// 从已验证的 MMIO 虚拟基址构造。
    ///
    /// # Safety
    ///
    /// `base` 必须指向有效的 DWC2 控制器 MMIO 区域（由平台代码保证）。
    /// 通常在平台初始化后调用一次，然后 `Box::leak` 为 `&'static`。
    pub unsafe fn new(base: usize) -> Self {
        Self { base }
    }

    /// 获取基址。
    #[inline]
    pub fn base(&self) -> usize {
        self.base
    }

    /// 获取 DWC2 全局寄存器视图。
    #[inline]
    pub fn regs(&self) -> &'static Dwc2Regs {
        unsafe { &*(self.base as *const Dwc2Regs) }
    }

    /// 获取第 `n` 个主机通道寄存器块（0-15）。
    ///
    /// Host Channel 寄存器在 MMIO 空间的偏移为 `0x500 + n * 0x20`。
    #[inline]
    pub fn host_channel(&self, n: usize) -> &'static Dwc2HostChannel {
        let offset = 0x500 + n * 0x20;
        unsafe { &*(self.base.wrapping_add(offset) as *const Dwc2HostChannel) }
    }
}
