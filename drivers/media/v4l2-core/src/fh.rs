//! V4L2 文件句柄——每次 open 的设备上下文。
//!
//! 对 `/dev/videoX` 节点的每次 `open()` 都会创建一个 `V4l2Fh`，
//! 保存每个 fd 的状态：事件订阅和待处理事件。
//! 对应 Linux 的 `struct v4l2_fh`。

use alloc::vec::Vec;
use core::sync::atomic::AtomicU32;

use crate::interface::event::{Event, EventSubscription};

/// 每个订阅者待处理事件队列的上限（对齐 Linux `V4L2_EVENT_Q_SIZE`）。
/// 队列满时丢弃最旧事件。
pub const EVENT_Q_SIZE: usize = 64;

/// 每次 open 对应的文件句柄——类似 Linux `struct v4l2_fh`。
///
/// 保存已订阅的事件类型，以及待处理事件队列。
///
/// # 限制
///
/// 当前 `VideoDevice` 为单 fh（所有 open 共享），订阅与事件队列不隔离；
/// per-open fh 需要 VFS 文件模型支持（DeviceOps 无 per-file 上下文），
/// 留作独立架构任务。
#[derive(Debug)]
pub struct V4l2Fh {
    /// 已订阅的事件规范（type、id、flags）。
    pub subscribed: Vec<EventSubscription>,
    /// 等待用户空间取出的待处理事件。
    pub pending: Vec<Event>,
    /// 用于分配事件 sequence 号的单调计数器。
    pub event_sequence: AtomicU32,
}

impl V4l2Fh {
    /// 创建新文件句柄。
    pub fn new() -> Self {
        Self {
            subscribed: Vec::new(),
            pending: Vec::new(),
            event_sequence: AtomicU32::new(0),
        }
    }

    /// 如果此 fh 已订阅给定的事件 type+id，则返回 true。
    ///
    /// `id == 0` 表示订阅该类型的所有事件（通配）。
    pub fn is_subscribed(&self, ty: u32, id: u32) -> bool {
        self.subscribed
            .iter()
            .any(|s| s.ty == ty && (s.id == id || s.id == 0))
    }

    /// 将待处理事件推入队列（FIFO：最新在尾部）。
    ///
    /// 分配单调递增的 sequence 号；队列满（[`EVENT_Q_SIZE`]）时丢最旧。
    pub fn push_event(&mut self, ev: &mut Event) {
        ev.sequence = self
            .event_sequence
            .fetch_add(1, core::sync::atomic::Ordering::Release)
            + 1;
        if self.pending.len() >= EVENT_Q_SIZE {
            self.pending.remove(0);
        }
        self.pending.push(*ev);
    }

    /// 弹出最早的待处理事件（FIFO：最早在头部）。
    pub fn pop_event(&mut self) -> Option<Event> {
        if self.pending.is_empty() {
            None
        } else {
            Some(self.pending.remove(0))
        }
    }
}

impl Default for V4l2Fh {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for V4l2Fh {
    fn drop(&mut self) {
        // 释放此 fh 持有的任何资源。
        // 目前只是清空向量。
        self.subscribed.clear();
        self.pending.clear();
    }
}
