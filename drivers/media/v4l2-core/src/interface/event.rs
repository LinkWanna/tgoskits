//! V4L2 事件类型 — v4l2_event、v4l2_event_subscription。
//!
//! 事件允许用户态订阅来自驱动的异步通知
//! （控制变化、格式变化、信号源变化等）。

use bitflags::bitflags;

use crate::interface::common::Timespec;

/// V4L2 事件订阅请求。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EventSubscription {
    pub ty: u32,              // [in] 事件类型（例如 [`EventType::Ctrl`]）
    pub id: u32,              // [in] 关联 ID（例如 [`EventType::Ctrl`] 对应的控制 ID）
    pub flags: EventSubFlags, // [in] 事件订阅标志
    pub reserved: [u32; 5],
}

/// 由用户态出队（dequeue）的 V4L2 事件。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Event {
    pub ty: u32,             // [out] 事件类型
    pub data: [u8; 64],      // [out] 事件负载
    pub pending: u32,        // [out] 该类型的待处理事件数量
    pub sequence: u32,       // [out] 单调递增序列号
    pub timestamp: Timespec, // [out] 事件时间戳
    pub id: u32,             // [out] 事件类型相关 ID（例如控制 ID）
    pub reserved: [u32; 8],
}

// ── 事件类型 ───────────────────────────────────────────────────────────

/// V4L2 事件类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    All          = 0,
    Vsync        = 1,
    Eos          = 2,
    Ctrl         = 3,
    FrameSync    = 4,
    SourceChange = 5,
    MotionDet    = 6,
    PrivateStart = 0x0800_0000,
}

impl EventType {
    /// 尝试将原始 `u32` 转换为 [`EventType`]。
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            0 => Self::All,
            1 => Self::Vsync,
            2 => Self::Eos,
            3 => Self::Ctrl,
            4 => Self::FrameSync,
            5 => Self::SourceChange,
            6 => Self::MotionDet,
            0x0800_0000 => Self::PrivateStart,
            _ => return None,
        })
    }
}

// ── 事件订阅标志 ─────────────────────────────────────────────

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EventSubFlags: u32 {
        /// 订阅时立即发送初始事件。
        const SEND_INITIAL = 1 << 0;
    }
}
