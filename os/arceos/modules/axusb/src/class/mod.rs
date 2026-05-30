//! USB Class Driver 实现。
//!
//! 每个子模块实现一种 USB 设备类协议：
//! - [`uvc`]：USB Video Class（摄像头）
//! - [`mass_storage`]：Mass Storage Class（U 盘）

pub mod mass_storage;
pub mod uvc;
