//! vivid — StarryOS 的虚拟视频驱动
//!
//! Linux `drivers/media/test-drivers/vivid/` 的 Rust 移植。
//! 模拟一个完整的 V4L2 视频采集 + 输出设备，具备：
//!
//! - 标准 V4L2 控件（brightness、contrast、saturation、hue 等）
//! - 通过 `tpg` 生成测试图案（独立实现，移植自 v4l2-tpg）
//! - 内存映射（mmap）流式 I/O
//!
//! ## 架构（镜像 Linux vivid）
//!
//! ```text
//! vivid-core       ← 结构体 VividDev（v4l2_device + media_device）
//!   ├── vivid-vid-cap  ← /dev/videoX：采集（生成测试图案）
//!   └── vivid-vid-out  ← /dev/videoY：输出（消费帧用于回环）
//! ```

#![no_std]

extern crate alloc;

pub mod ctrls;
pub mod tpg;
pub mod vid_cap;
pub mod vid_common;
pub mod vid_out;
