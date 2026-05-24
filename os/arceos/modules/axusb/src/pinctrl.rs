//! 用于处理 sg2002 cv181x pinctrl 的驱动
//! 目前没有任何的普适性，仅包含 sg2002 的寄存器地址和 USB 上电操作

use ax_hal::mem::{PhysAddr, phys_to_virt};
use sg200x_bsp::{
    gpio::{Direction, GPIO, GPIO1_BASE, GPIOPort},
    pinmux::{FMUX_USB_VBUS_DET, Pinmux},
};
use tock_registers::interfaces::Writeable;

const IOBLK_G1_PADDR: usize = 0x0300_1800;
const IOBLK_G1_USB_VBUS_DET_OFF: usize = 0x020;

const VBUS_GPIO_PORT: GPIOPort = GPIOPort::GPIO1;
const VBUS_GPIO_PIN: u8 = 6;
const VBUS_GPIO_ACTIVE_HIGH: bool = true;

pub fn pinmux_usb_vbus_det_gpio_output_prep() {
    let pinmux = Pinmux::new_with_offset(phys_to_virt(PhysAddr::from_usize(0)).as_usize());
    pinmux
        .fmux()
        .usb_vbus_det
        .write(FMUX_USB_VBUS_DET::FSEL::XGPIOB_6);
    let iob = phys_to_virt(PhysAddr::from_usize(IOBLK_G1_PADDR)).as_usize();
    let r = (iob + IOBLK_G1_USB_VBUS_DET_OFF) as *mut u32;
    unsafe {
        let v = core::ptr::read_volatile(r);
        core::ptr::write_volatile(r, v | (7 << 5));
    }
}

pub fn enable_usb_vbus_gpio() {
    let gpio_va = phys_to_virt(PhysAddr::from_usize(GPIO1_BASE)).as_usize();
    let gpio = unsafe { GPIO::from_base_address(gpio_va, VBUS_GPIO_PORT) };
    gpio.set_direction(VBUS_GPIO_PIN, Direction::Output);
    gpio.set(VBUS_GPIO_PIN, VBUS_GPIO_ACTIVE_HIGH);
}
