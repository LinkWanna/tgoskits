//! `dwc2_driver::Osal` 的 ArceOS 实现（SG2002/C906）。

use ax_hal::mem::{VirtAddr, virt_to_phys};
use dwc2_driver::Osal;

/// ArceOS 平台的 Osal 实现（SG2002/C906）。
pub struct AxOsal;

impl AxOsal {
    pub const fn new() -> Self {
        Self
    }
}

/// L1 数据缓存行大小（C906 = 64 字节）。
const CACHE_LINE: usize = 64;

/// C906 自定义 dcache clean 指令。
/// 编码与 OpenSBI / U-Boot `t-head_cache.S` 一致：funct12=0x025。
#[cfg(target_arch = "riscv64")]
#[inline(always)]
unsafe fn dcache_cva(va: usize) {
    unsafe {
        core::arch::asm!(".insn i 0x0b, 0, x0, {0}, 0x025", in(reg) va);
    }
}

/// C906 自定义 dcache invalidate 指令（funct12=0x026）。
#[cfg(target_arch = "riscv64")]
#[inline(always)]
unsafe fn dcache_iva(va: usize) {
    unsafe {
        core::arch::asm!(".insn i 0x0b, 0, x0, {0}, 0x026", in(reg) va);
    }
}

#[cfg(not(target_arch = "riscv64"))]
unsafe fn dcache_cva(_va: usize) {}
#[cfg(not(target_arch = "riscv64"))]
unsafe fn dcache_iva(_va: usize) {}

/// 清理（写回）一段地址范围的 dcache。
fn dcache_clean_range(va: usize, len: usize) {
    if len == 0 {
        return;
    }
    let start = va & !(CACHE_LINE - 1);
    let end = va + len;
    let mut addr = start;
    while addr < end {
        unsafe { dcache_cva(addr) };
        addr += CACHE_LINE;
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("fence iorw, iorw");
    }
}

/// 失效（丢弃）一段地址范围的 dcache。
fn dcache_invalidate_range(va: usize, len: usize) {
    if len == 0 {
        return;
    }
    let start = va & !(CACHE_LINE - 1);
    let end = va + len;
    let mut addr = start;
    while addr < end {
        unsafe { dcache_iva(addr) };
        addr += CACHE_LINE;
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("fence iorw, iorw");
    }
}

impl Osal for AxOsal {
    fn dma_alloc(&self, size: usize) -> Option<(*mut u8, u32, usize)> {
        let layout = core::alloc::Layout::from_size_align(size, CACHE_LINE).ok()?;
        let ptr = unsafe { alloc::alloc::alloc(layout) };
        if ptr.is_null() {
            return None;
        }
        let pa = self.virt_to_phys(ptr as usize);
        Some((ptr, pa, size))
    }

    unsafe fn dma_free(&self, ptr: *mut u8, size: usize) {
        let layout = core::alloc::Layout::from_size_align(size, CACHE_LINE).unwrap();
        unsafe { alloc::alloc::dealloc(ptr, layout) };
    }

    fn spin_udelay(&self, us: u32) {
        for _ in 0..us.saturating_mul(64) {
            core::hint::spin_loop();
        }
    }

    fn virt_to_phys(&self, va: usize) -> u32 {
        virt_to_phys(VirtAddr::from(va)).as_usize() as u32
    }

    fn dma_cache_clean(&self, va: *const u8, len: usize) {
        log::info!("dma_clean: va={:#018x} len={}", va as usize, len);
        dcache_clean_range(va as usize, len);
    }

    fn dma_cache_invalidate(&self, va: *const u8, len: usize) {
        log::info!("dma_inval: va={:#018x} len={}", va as usize, len);
        dcache_invalidate_range(va as usize, len);
    }
}
