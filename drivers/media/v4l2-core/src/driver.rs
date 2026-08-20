//! V4L2DriverOps — 完整的驱动对象 trait（VFS 操作 + 全部 ioctl）。
//!
//! `V4L2DriverOps` 对应 Linux 的 `struct v4l2_file_operations` 中
//! VFS 级回调（mmap、poll、release），并作为 [`IoctlOps`] 与
//! [`LegacyIoctlOps`] 的 supertrait：一个驱动对象同时持有
//! 非 ioctl 的 VFS 操作与全部 VIDIOC ioctl 回调，与 Linux
//! `video_device` 同时挂载 `v4l2_file_operations` 和
//! `v4l2_ioctl_ops` 的结构一致。

use alloc::{sync::Arc, vec::Vec};

use axpoll::PollSet;

use crate::ioctl::{IoctlOps, LegacyIoctlOps};

/// 完整 V4L2 驱动对象：非 ioctl 的 VFS 操作 + 全部 ioctl 回调。
///
/// 非 ioctl 操作：内存映射、轮询与生命周期（对应 Linux
/// `v4l2_file_operations`）。继承 [`IoctlOps`]（modern）与
/// [`LegacyIoctlOps`]（遗留），因此单个 `dyn V4L2DriverOps`
/// 即可按命令路由到任意 ioctl trait。
#[allow(unused_variables)]
pub trait V4L2DriverOps: Send + Sync + IoctlOps + LegacyIoctlOps {
    /// 将用户态 mmap 偏移解析为物理地址（mmap 偏移解码）。
    ///
    /// 由 `Vb2Queue` 支撑的驱动一行委托给 `Vb2Queue::mmap` 即可——
    /// 不要重新实现偏移解码（stride 编码由分配器决定）。
    fn mmap(&self, offset: u64, length: u64) -> Option<(Vec<usize>, usize)> {
        None
    }

    /// poll 就绪：有数据可读时返回 `true`
    /// （即 DQBUF 不会被阻塞）。与媒体类型无关——名字
    fn is_readable(&self) -> bool {
        false
    }

    /// poll 错误：队列进入错误状态（DQBUF 将返回 `Io` 错误）。
    ///
    /// 为 true 时 VFS poll 报告 `POLLERR`。
    fn is_error(&self) -> bool {
        false
    }

    /// 完成事件唤醒源（DQBUF 阻塞与 VFS poll 共用）。
    fn vb_poll_set(&self) -> Option<Arc<PollSet>> {
        None
    }

    /// 当指向此设备的最后一个文件描述符关闭时调用。
    fn release(&self) {}
}
