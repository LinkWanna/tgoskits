//! V4L2 文件句柄——每次 open 的设备上下文与事件队列。
//!
//! 对 `/dev/videoX` 节点的每次 `open()` 都会创建一个 [`V4l2Fh`]，
//! 保存每个 fd 的事件订阅和待处理事件。对应 Linux 的 `struct v4l2_fh`。
//!
//! 事件队列算法对应 Linux `v4l2-event.c`：每个订阅拥有独立的环形队列
//! （[`SubscribedEvent`]），队列满时按 [`EventOps`] 合并或丢弃最旧事件；
//! 序列号在 fh 上单调递增（初始 `-1`，首个事件 sequence = 0）。出队保持
//! 全局 FIFO：跨订阅按事件 sequence 取最旧者，与 Linux 单一
//! `fh->available` 链表顺序一致。

use alloc::{collections::VecDeque, vec::Vec};

use crate::{
    Result, V4l2Error,
    interface::event::{CtrlChange, Event, EventCtrlPayload, EventSubscription, EventType},
};

/// 订阅队列长度默认值。
pub const EVENT_QUEUE_DEFAULT_ELEMS: usize = 1;

/// 订阅事件队列溢出时的合并策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventOps {
    DropOldest,
    Ctrl,
}

/// `SubscribedEvent::push` 的结果：是否新增了待处理槽位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    Inserted,
    Merged,
}

/// `V4l2Fh::queue_event` 的投递结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueOutcome {
    Delivered,
    Merged,
    NoSubscription,
}

// ── 核心类型 ─────────────────────────────────────────────────────────

/// V4L2 文件句柄——每次 open 的设备上下文.
///
/// 保存每次 `open()` 的独立状态：事件订阅、待处理事件与序列号。
/// 订阅与队列算法直接作为方法实现（对齐 Linux `v4l2-event.c`）。
#[derive(Debug)]
pub struct V4l2Fh {
    /// 已订阅的事件（每订阅独立环形队列）。
    subscribed: Vec<SubscribedEvent>,
    /// fh 级事件序列号（初始 `0xFFFF_FFFF` 即 -1，首个事件 sequence = 0）。
    sequence: u32,
    /// 所有订阅累计的待处理事件数（对齐 Linux `fh->navailable`）。
    pending_count: usize,
}

impl V4l2Fh {
    /// 创建新文件句柄。
    pub fn new() -> Self {
        Self {
            subscribed: Vec::new(),
            sequence: u32::MAX,
            pending_count: 0,
        }
    }

    /// 订阅一种事件类型。
    ///
    /// `type = ALL` 与 `id` 无关，不能作为订阅类型，返回 `InvalidArgument`。
    /// 重复订阅同一 `type+id` 是幂等的（返回 `Ok` 且不重复投递初始事件）。
    pub fn subscribe(&mut self, sub: &EventSubscription) -> Result<()> {
        if sub.ty == EventType::All {
            return Err(V4l2Error::InvalidArgument);
        }
        if self.is_subscribed(sub.ty, sub.id) {
            return Ok(());
        }
        let (elems, ops) = Self::subscription_params(sub.ty);
        self.subscribed.push(SubscribedEvent {
            ty: sub.ty,
            id: sub.id,
            ops,
            elems,
            events: VecDeque::with_capacity(elems),
        });
        Ok(())
    }

    /// 订阅一种事件类型（兼容旧 `elems/ops` 裸参路径）。
    ///
    /// 仅保留以兼容历史调用点；新代码应直接使用 [`Self::subscribe`]，
    /// 其内部已按 `EventType` 推导 `elems/ops`。
    pub fn subscribe_with_params(
        &mut self,
        sub: &EventSubscription,
        elems: usize,
        ops: EventOps,
    ) -> Result<()> {
        if sub.ty == EventType::All {
            return Err(V4l2Error::InvalidArgument);
        }
        if self.is_subscribed(sub.ty, sub.id) {
            return Ok(());
        }
        let elems = elems.max(EVENT_QUEUE_DEFAULT_ELEMS);
        self.subscribed.push(SubscribedEvent {
            ty: sub.ty,
            id: sub.id,
            ops,
            elems,
            events: VecDeque::with_capacity(elems),
        });
        Ok(())
    }

    /// 取消订阅；`type = ALL` 时取消全部订阅。
    ///
    /// 取消未订阅的 `type+id` 无副作用（幂等，对齐 Linux
    /// `v4l2_event_unsubscribe`）。
    pub fn unsubscribe(&mut self, sub: &EventSubscription) {
        if sub.ty == EventType::All {
            self.unsubscribe_all();
            return;
        }
        let Some(pos) = self
            .subscribed
            .iter()
            .position(|s| s.ty == sub.ty && s.id == sub.id)
        else {
            return;
        };
        let sev = self.subscribed.remove(pos);
        self.pending_count = self.pending_count.saturating_sub(sev.events.len());
        debug_assert_eq!(
            self.pending_count,
            self.subscribed.iter().map(|s| s.events.len()).sum()
        );
    }

    /// 投递一个事件：仅分发给精确匹配 `type+id` 的订阅，并分配 `sequence`。
    ///
    /// 返回投递结果；`Delivered/Merged` 均表示已找到订阅（`Merged` 时
    /// 队列内合并，不新增 `pending`），`NoSubscription` 表示无匹配而丢弃。
    /// 投递成功后由调用方唤醒 `poll` 的 `POLLPRI` 等待者。
    pub fn queue_event(&mut self, mut ev: Event) -> QueueOutcome {
        let Some(idx) = self
            .subscribed
            .iter()
            .position(|s| s.ty as u32 == ev.ty && s.id == ev.id)
        else {
            return QueueOutcome::NoSubscription;
        };
        ev.sequence = self.alloc_sequence();
        let sev = &mut self.subscribed[idx];
        let outcome = sev.push(ev);
        if outcome == PushOutcome::Inserted {
            self.pending_count += 1;
            QueueOutcome::Delivered
        } else {
            QueueOutcome::Merged
        }
    }

    /// 兼容旧 `bool` 返回的投递接口（`true`=已找到订阅）。
    pub fn queue_event_bool(&mut self, ev: Event) -> bool {
        !matches!(self.queue_event(ev), QueueOutcome::NoSubscription)
    }

    /// 取出一条待处理事件（非阻塞；对齐 Linux 非阻塞 `v4l2_event_dequeue`）。
    ///
    /// 跨订阅按 `sequence` 取最旧者，保持全局 FIFO（对齐 Linux `available`
    /// 链表）。无待处理事件时返回 `NoEntry`（`ENOENT`）。
    pub fn dequeue(&mut self) -> Result<Event> {
        let idx = self
            .subscribed
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.events.front().map(|e| (i, e.sequence)))
            .min_by_key(|&(_, seq)| seq)
            .map(|(i, _)| i)
            .ok_or(V4l2Error::NoEntry)?;
        let sev = &mut self.subscribed[idx];
        let mut ev = sev.events.pop_front().expect("front selected above");
        self.pending_count = self.pending_count.saturating_sub(1);
        ev.pending = self.pending_count as u32;
        debug_assert_eq!(
            self.pending_count,
            self.subscribed.iter().map(|s| s.events.len()).sum()
        );
        Ok(ev)
    }

    /// 当前待处理事件数（`poll` 路径判 `POLLPRI`）。
    pub fn pending(&self) -> usize {
        self.pending_count
    }

    /// 是否已订阅给定 `type+id`。
    pub fn is_subscribed(&self, ty: EventType, id: u32) -> bool {
        self.subscribed.iter().any(|s| s.ty == ty && s.id == id)
    }

    /// 分配下一个 `sequence`（`wrapping_add`，初始 `MAX`→`0`）。
    fn alloc_sequence(&mut self) -> u32 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }

    /// 按 `EventType` 推导订阅参数（`elems/ops`）。
    fn subscription_params(ty: EventType) -> (usize, EventOps) {
        match ty {
            EventType::Ctrl => (EVENT_QUEUE_DEFAULT_ELEMS, EventOps::Ctrl),
            _ => (EVENT_QUEUE_DEFAULT_ELEMS, EventOps::DropOldest),
        }
    }

    /// 取消全部订阅并清空待处理事件。
    fn unsubscribe_all(&mut self) {
        self.pending_count = 0;
        self.subscribed.clear();
    }
}

impl Default for V4l2Fh {
    fn default() -> Self {
        Self::new()
    }
}

// ── 订阅队列细节 ─────────────────────────────────────────────────────

/// 一次订阅及其环形事件队列（Linux `struct v4l2_subscribed_event`）。
#[derive(Debug)]
struct SubscribedEvent {
    ty: EventType,
    id: u32,
    ops: EventOps,
    elems: usize,
    events: VecDeque<Event>,
}

impl SubscribedEvent {
    /// 入队一个事件；队列满时在槽位内合并/替换。
    ///
    /// 未满时 `push_back` 并返回 `Inserted`；满时按 `EventOps::Ctrl`
    /// 的单槽替换/多槽合并策略处理，返回 `Merged`（`pending` 不新增）。
    fn push(&mut self, ev: Event) -> PushOutcome {
        if self.events.len() < self.elems {
            self.events.push_back(ev);
            return PushOutcome::Inserted;
        }
        let mut oldest = self.events.pop_front().expect("full queue is non-empty");
        if self.elems == 1 && self.ops == EventOps::Ctrl {
            ctrl_replace(&mut oldest, &ev);
            oldest.ty = ev.ty;
            oldest.id = ev.id;
            oldest.sequence = ev.sequence;
            oldest.timestamp = ev.timestamp;
            self.events.push_back(oldest);
        } else {
            if self.elems > 1
                && self.ops == EventOps::Ctrl
                && let Some(newest) = self.events.back_mut()
            {
                ctrl_merge(&oldest, newest);
            }
            self.events.push_back(ev);
        }
        PushOutcome::Merged
    }
}

// ── CTRL 事件载荷 ────────────────────────────────────────────────────

/// `v4l2_ctrl_replace`：用新事件载荷替换 `old`，并把旧 `changes` 按或合并。
fn ctrl_replace(old: &mut Event, new: &Event) {
    let old_changes = EventCtrlPayload::read_from(old).changes;
    let mut payload = EventCtrlPayload::read_from(new);
    payload.changes |= old_changes;
    payload.write_into(&mut old.data);
}

/// `v4l2_ctrl_merge`：把 `old` 的 `changes` 按或合并进 `new`。
fn ctrl_merge(old: &Event, new: &mut Event) {
    let mut payload = EventCtrlPayload::read_from(new);
    payload.changes |= EventCtrlPayload::read_from(old).changes;
    payload.write_into(&mut new.data);
}

/// 构建 V4L2_EVENT_CTRL 事件所需的控件元数据与当前值。
#[derive(Debug, Clone, Copy)]
pub struct CtrlEventParams {
    pub id: u32,
    pub ctrl_type: u32,
    pub value: i64,
    pub flags: u32,
    pub minimum: i64,
    pub maximum: i64,
    pub step: i64,
    pub default_value: i64,
}

/// 构建一个 `V4L2_EVENT_CTRL` 事件（对齐 Linux `v4l2_ctrls-core.c::fill_event`）。
///
/// 供 [`crate::ctrls::CtrlHandler`] 在订阅初始事件与值变化时调用。
pub fn build_ctrl_event(params: CtrlEventParams, changes: CtrlChange) -> Event {
    let ctrl = EventCtrlPayload {
        changes: changes.bits(),
        ty: params.ctrl_type,
        value: params.value as u64,
        flags: params.flags,
        minimum: params.minimum as i32,
        maximum: params.maximum as i32,
        step: params.step as i32,
        default_value: params.default_value as i32,
    };
    let mut data = [0u8; 64];
    ctrl.write_into(&mut data);
    Event {
        ty: EventType::Ctrl as u32,
        pad: 0,
        data,
        pending: 0,
        sequence: 0,
        timestamp: crate::interface::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        id: params.id,
        reserved: [0; 8],
    }
}
