//! 这里提供 USB 协议栈
//! 由于目前没有 pinctrl 和 phy 的抽象因此暂时将它们放在一起实现

#![no_std]

use ax_driver::{AxDeviceContainer, prelude::*};
use ax_lazyinit::LazyInit;
use ax_sync::Mutex;
use sg200x_bsp::usb::host::TopologyScanExtras;

use crate::{
    phy::{cvitek_usb_top_host_bringup, enable_usb_clocks_cv181x},
    pinctrl::{enable_usb_vbus_gpio, pinmux_usb_vbus_det_gpio_output_prep},
};

#[macro_use]
extern crate log;
extern crate alloc;

pub mod class;
pub mod imgcat;
mod phy;
mod pinctrl;
pub mod topology;
// pub mod usb_camera;

/// 辅助函数：近似的微秒级忙等待
#[inline]
fn spin_udelay_approx(us: u32) {
    for _ in 0..us.saturating_mul(64) {
        core::hint::spin_loop();
    }
}

static MAIN_USB: LazyInit<Mutex<AxUsbDevice>> = LazyInit::new();

pub fn init_usb(mut usb_devs: AxDeviceContainer<AxUsbDevice>) {
    info!("Initialize usb subsystem...");

    // 初始化 PHY
    unsafe {
        enable_usb_clocks_cv181x();
        cvitek_usb_top_host_bringup();
    }

    // 初始化 pinctrl
    pinmux_usb_vbus_det_gpio_output_prep();
    enable_usb_vbus_gpio();
    spin_udelay_approx(2_000_000);

    // 初始化 USB 驱动
    if let Some(mut dev) = usb_devs.take_one() {
        info!("  use usb device 0: {:?}", dev.device_name());
        dev.init().unwrap();
        MAIN_USB.init_once(Mutex::new(dev));
    } else {
        warn!("  No usb device found!");
    }
}

pub fn has_usb() -> bool {
    MAIN_USB.is_inited()
}

pub fn device_list() -> TopologyScanExtras {
    MAIN_USB
        .lock()
        .device_list()
        .expect("Failed to get USB device list")
}
