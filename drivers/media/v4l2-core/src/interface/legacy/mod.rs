//! 遗留接口的 ABI 类型。
//!
//! 对应 Linux `v4l2_ioctl_ops` 中已废弃/新设备不再实现的 ioctl 参数结构：
//! Overlay 帧缓冲（framebuffer）、模拟电视标准（standard）、Tuner/Radio
//! （tuner）、调制器（modulator）、音频 I/O（audio）、JPEG 压缩旧 API
//! （jpegcomp）、Sliced VBI（vbi）、stateful codec（codec）与调试寄存器
//! （debug）。这些接口经 [`crate::ioctl::LegacyIoctlCmd`] 路由到
//! [`crate::ioctl::LegacyIoctlOps`]，驱动默认无需实现。

pub mod audio;
pub mod codec;
pub mod debug;
pub mod framebuffer;
pub mod jpegcomp;
pub mod modulator;
pub mod standard;
pub mod tuner;
pub mod vbi;
