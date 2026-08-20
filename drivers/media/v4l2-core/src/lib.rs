//! StarryOS 的 V4L2 核心框架。

#![no_std]

extern crate alloc;

pub mod ctrls;
pub mod device;
pub mod driver;
pub mod error;
pub mod filehandler;
pub mod interface;
pub mod ioctl;
pub mod uapi;

pub use driver::V4L2DriverOps;
pub use error::{Result, V4l2Error};
pub use ioctl::{IoctlCmd, IoctlOps, LegacyIoctlCmd, LegacyIoctlOps};
