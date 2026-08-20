//! V4L2DriverOps——非 ioctl 的驱动操作。
//!
//! 这些对应 Linux 通过 `struct v4l2_file_operations` 暴露的
//! VFS 级回调（mmap、poll、release）。它们与 [`IoctlOps`] 分开，
//! 因为它们是 VFS 操作，而非 ioctl 命令。

use alloc::{sync::Arc, vec::Vec};

use axpoll::PollSet;

/// 非 ioctl 驱动操作：内存映射、轮询与生命周期。
#[allow(unused_variables)]
pub trait V4L2DriverOps: Send + Sync {
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
