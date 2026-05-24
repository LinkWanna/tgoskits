//! 负责接入外部的 DWC2 驱动，提供 USB 主机功能。
//!
//! sg200x-bsp 中的 SUB 驱动并没有将 USB host 抽象成一个结构体提供给上层调用
//! 同时使用 `set_usb_dma_to_phys_fn` 这样的函数注册方式，尽管不是很适合在内核
//! 驱动中使用，但是为了兼容现有的 DWC2 驱动，我们暂时只能在驱动初始化阶段调用这些函数来设置全局状态

use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use ax_hal::mem::{PhysAddr, VirtAddr, phys_to_virt, virt_to_phys}; /* 不应该依赖 ax_hal，但是目前简化地址 */
use sg200x_bsp::usb::{
    host::{self, TopologyScanExtras, dwc2},
    log, platform,
};

use crate::UsbDriverOps;

#[inline]
fn spin_udelay_approx(us: u32) {
    for _ in 0..us.saturating_mul(64) {
        core::hint::spin_loop();
    }
}

fn ep0_dma_virt_to_phys(p: *const u8) -> u32 {
    virt_to_phys(VirtAddr::from(p as usize)).as_usize() as u32
}

fn usb_log_line(s: &str) {
    debug!("{s}");
}

pub struct Dwc2 {
    dwc2_base: usize,
}

impl Dwc2 {
    /// Creates a new [`Dwc2`] from the given base address.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `base` is a valid pointer to the DWC2 controller's
    /// register block and that no other code is concurrently accessing the same hardware.
    pub fn new(dwc2_base: usize) -> Self {
        Self { dwc2_base }
    }
}

impl BaseDriverOps for Dwc2 {
    fn device_name(&self) -> &str {
        "DWC2 USB Host Controller"
    }

    fn device_type(&self) -> ax_driver_base::DeviceType {
        DeviceType::Usb
    }
}

impl UsbDriverOps for Dwc2 {
    /// 初始化 DWC2 驱动，设置必要的全局状态
    fn init(&mut self) -> DevResult<()> {
        platform::set_dwc2_base_virt(self.dwc2_base);
        platform::set_usb_dma_to_phys_fn(Some(ep0_dma_virt_to_phys));
        log::set_usb_log_fn(usb_log_line);
        dwc2::ep0::debug_log_ep0_dma_info();

        unsafe {
            dwc2::dwc2_probe().map_err(|e| {
                error!("USB DWC2 probe failed: {e:?}");
                DevError::Io
            })?;
        }

        Ok(())
    }

    /// 枚举 USB 设备，重试机制以应对偶发的枚举失败（如 PHY 切换未完成）
    fn device_list(&mut self) -> DevResult<TopologyScanExtras> {
        let mut last_err = None;
        let extras = (0..4)
            .find_map(|attempt| {
                // 这里需要延时，目前 ArceOS 还没有定时接口，只能轮询
                if attempt > 0 {
                    spin_udelay_approx(1_500_000 * attempt as u32);
                }
                match host::enumerate_topology_only() {
                    Ok(ex) => Some(ex),
                    Err(e) => {
                        warn!("USB: 枚举失败 #{}: {:?}", attempt + 1, e);
                        last_err = Some(e);
                        None
                    }
                }
            })
            .ok_or_else(|| {
                warn!("USB: 枚举重试全部失败: {:?}", last_err);
                DevError::Unsupported
            })?;

        Ok(extras)
    }
}
