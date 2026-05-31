//! DWC2 USB 主机控制器驱动（纯硬件层）。
//!
//! # 架构
//!
//! 本 crate 仅依赖 `tock-registers` + `log` + `usb-if`（纯类型），
//! 不依赖任何 OS crate。所有平台相关能力通过 [`Osal`] trait 注入。
//!
//! # 使用方式
//!
//! ```ignore
//! // 1. 构造控制器
//! let mmio_base = 0x0434_0000; // SG2002
//! let mut ctrl = unsafe { Dwc2Controller::new(mmio_base, &MY_OSAL) };
//!
//! // 2. 初始化硬件
//! ctrl.hw_init()?;
//! ctrl.alloc_dma_buf(384 * 1024)?;
//!
//! // 3. 等待设备
//! let speed = ctrl.wait_connect()?;
//! ctrl.root_port_reset(speed)?;
//!
//! // 4. EP0 控制传输
//! ctrl.ep0_control_in(0, &setup_bytes, &mut buf, 64)?;
//! ```
//!
//! # 分层
//!
//! ```text
//! axdriver_usb (OS 适配层)  ← 实现 Osal trait, 封装 Dwc2Controller
//! dwc2-driver  (纯硬件层)    ← 本 crate
//! ```

#![no_std]

extern crate alloc;

#[macro_use]
extern crate log;

pub mod channel;
pub mod controller;
pub mod device;
pub mod dma;
pub mod endpoint;
pub mod err;
pub mod mmio;
pub mod osal;
pub mod reg;
pub mod speed;

// Re-export 常用类型
pub use channel::{EpType, HostChannel};
pub use controller::Dwc2Controller;
pub use device::Dwc2Device;
pub use dma::DmaBuffer;
pub use endpoint::Dwc2Endpoint;
pub use err::{Error, Result};
pub use mmio::Dwc2Mmio;
pub use osal::Osal;
pub use speed::Speed;
