//! 驱动所需的 OS 抽象层 trait。
//!
//! DWC2 驱动自身 `#![no_std]`，所有平台相关能力通过此 trait 注入。
//! 上层（`axdriver_usb`）负责提供具体实现。

/// 驱动所需的 OS 能力抽象。
///
/// # Safety
///
/// 实现者必须保证：
/// - `dma_alloc` 返回的物理地址是真实的总线地址（DWC2 内部 DMA 使用）
/// - `virt_to_phys` 与 `dma_alloc` 使用相同的地址空间视角
pub trait Osal: Send + Sync {
    /// 分配物理连续的 DMA 缓冲区。
    ///
    /// 返回（虚拟地址，物理地址，字节大小）。
    /// 虚拟地址用于 CPU 读写，物理地址写入 HCDMA 寄存器。
    fn dma_alloc(&self, size: usize) -> Option<(*mut u8, u32, usize)>;

    /// 释放 DMA 缓冲区。
    ///
    /// # Safety
    ///
    /// 调用者保证 `ptr` 来自此 `Osal` 的上一次 `dma_alloc` 调用，
    /// 且该缓冲区不再被硬件访问。
    unsafe fn dma_free(&self, ptr: *mut u8, size: usize);

    /// 微秒级忙等待（近似即可）。
    fn spin_udelay(&self, us: u32);

    /// 毫秒级忙等待。
    fn spin_mdelay(&self, ms: u32) {
        for _ in 0..ms {
            self.spin_udelay(1000);
        }
    }

    /// 虚拟地址 → 物理地址。
    ///
    /// 用于将栈/堆上的数据传输缓冲区转换为 DMA 地址。
    fn virt_to_phys(&self, va: usize) -> u32;

    /// 清理（写回）CPU 缓存到内存，确保 DMA 引擎读到最新数据。
    ///
    /// 在 DMA 从该区域**读取**之前调用（OUT 传输）。
    fn dma_cache_clean(&self, _va: *const u8, _len: usize) {}

    /// 失效（丢弃）CPU 缓存，确保 CPU 读到 DMA 写入的最新数据。
    ///
    /// 在 DMA 向该区域**写入**之后调用（IN 传输）。
    fn dma_cache_invalidate(&self, _va: *const u8, _len: usize) {}
}

/// 默认的空 OS 实现（仅用于编译测试，不可用于实际硬件）。
///
/// 所有方法 panic — 仅用于确保 trait 签名在不同平台间一致。
#[doc(hidden)]
pub struct NopOsal;

impl Osal for NopOsal {
    fn dma_alloc(&self, _size: usize) -> Option<(*mut u8, u32, usize)> {
        None
    }

    unsafe fn dma_free(&self, _ptr: *mut u8, _size: usize) {}

    fn spin_udelay(&self, _us: u32) {}

    fn virt_to_phys(&self, _va: usize) -> u32 {
        0
    }
}
