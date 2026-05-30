//! SG2002 cv181x USB PHY 初始化。
//!
//! 仅包含 SG2002 的寄存器地址和 USB 主机模式 bring-up 操作。
//! 不具普适性，仅用于 DWC2 后端。

use ax_hal::mem::{PhysAddr, phys_to_virt};

const CLKGEN_PADDR: usize = 0x0300_2000;
const TOP_PADDR: usize = 0x0300_0000;

/// 使能 cv181x USB 相关时钟。
///
/// # Safety
///
/// 调用者需确保 MMU 已启用且物理地址可被 `phys_to_virt` 映射。
pub unsafe fn enable_usb_clocks_cv181x() {
    let b = phys_to_virt(PhysAddr::from_usize(CLKGEN_PADDR)).as_usize();
    let en1 = (b + 0x004) as *mut u32;
    let en2 = (b + 0x008) as *mut u32;
    let byp0 = (b + 0x030) as *mut u32;

    unsafe {
        let v1_pre = core::ptr::read_volatile(en1);
        let v2_pre = core::ptr::read_volatile(en2);
        let byp_pre = core::ptr::read_volatile(byp0);
        core::ptr::write_volatile(en1, v1_pre | (0xFu32 << 28));
        core::ptr::write_volatile(en2, v2_pre | 1u32);
        core::ptr::write_volatile(byp0, byp_pre & !((1u32 << 17) | (1u32 << 18)));
    }
}

/// PHY ID pad toggle workaround（见 phy-cv1800-usb.c）：先写 device 再写 host。
///
/// # Safety
///
/// 调用者在 PHY 初始化阶段单线程调用，确保无并发访问。
pub unsafe fn cvitek_usb_top_host_bringup() {
    let top = phys_to_virt(PhysAddr::from_usize(TOP_PADDR)).as_usize();
    let rst = (top + 0x3000) as *mut u32;
    unsafe {
        let v = core::ptr::read_volatile(rst);
        core::ptr::write_volatile(rst, v & !(1 << 11));
        core::ptr::write_volatile(rst, v | (1 << 11));

        let usb_pin = (top + 0x48) as *mut u32;
        let x = core::ptr::read_volatile(usb_pin);
        let dev_mode = (x & !0xC0u32) | 0xC0u32 | 0x01u32;
        core::ptr::write_volatile(usb_pin, dev_mode);
        let host_mode = (x & !0xC0u32) | 0x40u32 | 0x01u32;
        core::ptr::write_volatile(usb_pin, host_mode);

        let eco = (top + 0xB4) as *mut u32;
        core::ptr::write_volatile(eco, core::ptr::read_volatile(eco) | 0x80);
    }
}
