//! V4L2 buffer 队列框架（vb2——Video Buffer 2）。
//!
//! 对应 Linux 的 `drivers/media/common/videobuf2/`。
//! 实现 buffer 状态机（DEQUEUED → QUEUED → ACTIVE → DONE），
//! 通过 [`Vb2MemOps`] 提供可插拔的内存分配，
//! 通过 [`Vb2Ops`] 提供驱动回调。
//!
//! ## 架构
//!
//! ```text
//! Vb2Queue<M>                    ← 状态机（no_std）
//!
//! Vb2MemOps 的实现：
//! ```
//!
//! # 锁约定
//!
//! 所有公共方法都接受 `&self` 并在内部
//! `SpinLock` 锁上串行化，因此队列可在任务上下文
//! （ioctl / 采集 worker）之间安全共享。
//! `Vb2Ops` 回调在**持有内部锁时**被调用：
//! 实现不得重入队列，且必须保持
//! 临界区尽量短。
//! 完成唤醒（`poll_rx`）总在内部锁**释放后**发出。

#![no_std]
#[cfg(test)]
extern crate std;

extern crate alloc;

pub mod allocator;
pub mod buf;

use alloc::{collections::VecDeque, sync::Arc, vec::Vec};

pub use allocator::{Vb2MemOps, VirtualAllocator};
use ax_sync::SpinLock;
use axpoll::{IoEvents, PollSet};
pub use buf::{BufferState, MemPlane, VIDEO_MAX_PLANES, Vb2Buffer};
use v4l2_core::{V4l2Error, interface::buffer::BufFlags};

/// 由内部锁保护的状态。
struct QueueInner {
    buffers: Vec<Vb2Buffer>,
    queued_list: VecDeque<u32>,
    done_list: VecDeque<u32>,

    owned_by_drv_count: u32,
    sequence: u32,
    streaming: bool,
    start_streaming_called: bool,
    error: bool,
}

impl QueueInner {
    /// 无锁版本：`buf_prepare` + 标记 prepared（qbuf/prepare_buf 共用）。
    fn __prepare(vb: &mut Vb2Buffer) -> Result<(), V4l2Error> {
        if vb.state != BufferState::Dequeued || vb.prepared {
            return Err(V4l2Error::InvalidArgument);
        }
        vb.prepared = true;
        Ok(())
    }

    fn __enqueue_in_driver(&mut self, index: u32) {
        let vb = &mut self.buffers[index as usize];
        vb.state = BufferState::Active;
        self.owned_by_drv_count += 1;
    }

    fn __start_streaming_now(&mut self) {
        let queued_indices: Vec<u32> = self.queued_list.iter().copied().collect();
        for idx in queued_indices {
            self.__enqueue_in_driver(idx);
        }
        self.start_streaming_called = true;
    }
}

/// V4L2 buffer 队列——vb2 状态机。
///
/// 线程安全：所有方法都接受 `&self` 并在内部
/// `SpinLock` 锁上串行化。[`buffer_done`](Self::buffer_done) 由采集
/// 侧（任务上下文）调用。
pub struct Vb2Queue<M: Vb2MemOps> {
    inner: SpinLock<QueueInner>,
    alloc: M,
    /// 完成事件唤醒源（对齐 Linux vb2 `done_wq`）：`buffer_done`/`set_error`
    /// 在状态发布后唤醒，DQBUF 阻塞等待与 VFS poll 共用。
    poll_rx: Arc<PollSet>,
    /// reqbufs 的缓冲数上下限（驱动配置；替代 Linux vb2_ops::queue_setup
    /// 的 clamp 职责——当前所有驱动只用它限制缓冲数）。
    min_buffers: u32,
    max_buffers: u32,
}

impl<M: Vb2MemOps> Vb2Queue<M> {
    pub fn new(alloc: M, min_buffers: u32, max_buffers: u32) -> Self {
        Self {
            inner: SpinLock::new(QueueInner {
                buffers: Vec::new(),
                queued_list: VecDeque::new(),
                done_list: VecDeque::new(),
                owned_by_drv_count: 0,
                sequence: 0,
                streaming: false,
                start_streaming_called: false,
                error: false,
            }),
            alloc,
            poll_rx: Arc::new(PollSet::new()),
            min_buffers,
            max_buffers,
        }
    }

    // ── 驱动侧（采集回调）──────────────────────────────────────────

    /// 将 buffer 标记为 done/error。由采集侧（任务上下文，如流
    /// worker）调用。
    ///
    /// 若 `index` 越界、buffer 当前不是
    /// `Active`，或 `state` 不是 `Done`/`Error`——驱动
    /// 不得静默忽略拒绝。
    pub fn buffer_done(
        &self,
        index: u32,
        state: BufferState,
        bytesused: u32,
        field: u32,
    ) -> Result<(), V4l2Error> {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        let vb = inner
            .buffers
            .get_mut(index as usize)
            .ok_or(V4l2Error::InvalidArgument)?;

        if vb.state != BufferState::Active {
            return Err(V4l2Error::InvalidArgument);
        }
        if state != BufferState::Done && state != BufferState::Error {
            return Err(V4l2Error::InvalidArgument);
        }

        vb.bytesused = bytesused;
        vb.field = field;
        vb.timestamp = ax_runtime::hal::time::monotonic_time_nanos();
        vb.timestamp_flags = BufFlags::TIMESTAMP_MONOTONIC.bits();
        inner.sequence += 1;
        let seq = inner.sequence;
        vb.sequence = seq;
        vb.state = state;
        inner.done_list.push_back(index);
        inner.owned_by_drv_count -= 1;
        drop(guard);
        // 就绪状态已在锁内发布，此处锁外唤醒（锁序固定 queue.inner →
        // pollset.inner，单向无环）。
        self.poll_rx.wake_from_irq(IoEvents::IN);
        Ok(())
    }
    // ── mmap 解码 ───────────────────────────────────────────────

    /// 将用户空间的 mmap 偏移映射为逐页物理地址列表。
    ///
    /// 按每个 buffer 的 mmap 偏移（`MemPlane::offset`，分配器在
    /// `alloc` 时按自己的 stride 布局填入）解码 `offset`，而不假设
    /// 任何固定 stride；物理页由分配器
    /// [`Vb2MemOps::mmap`] 提供。
    /// 范围无效或超出 plane 边界时返回
    /// `Some((页列表, len))` 或 `None`。
    ///
    /// 单 plane 语义：只考虑每个 buffer 的第一个 plane
    /// （与当前 vivid/uvc 用法一致；多 plane 的 mmap
    /// 偏移编码尚未实现）。
    /// 解码用户 mmap 偏移 → 逐页物理地址列表。布局由分配器持有
    /// （`MemPlane::offset` 是 UAPI 偏移、`[`Vb2MemOps::mmap`] 提供
    /// 逐页映射）——队列只做 offset → buffer/plane 匹配与页切片。
    pub fn mmap(&self, offset: u64, length: u64) -> Option<(Vec<usize>, usize)> {
        let inner = self.inner.lock();
        for vb in inner.buffers.iter() {
            let plane = vb.planes.first()?;
            let base = plane.offset as u64;
            let end = base + plane.length as u64;
            if offset >= base && offset < end {
                if offset + length > end {
                    return None;
                }
                let sub = (offset - base) as usize;
                let page = 4096usize;
                let first_page = sub / page;
                let n_pages = (sub % page + length as usize).div_ceil(page);
                let all = self.alloc.mmap(plane);
                let addrs = all
                    .get(first_page..first_page + n_pages)
                    .unwrap_or_default()
                    .to_vec();
                return Some((addrs, length as usize));
            }
        }
        None
    }
    // ── poll 支撑 ──────────────────────────────────────────────────

    /// 完成事件唤醒源：DQBUF 阻塞等待与 VFS poll 共用（对齐 Linux
    /// vb2 `done_wq` 单 waitqueue 服务两者）。
    pub fn poll_set(&self) -> &Arc<PollSet> {
        &self.poll_rx
    }

    /// done_list 是否有完成的帧可 dqbuf（poll 路径：POLLIN 就绪检查）。
    pub fn is_readable(&self) -> bool {
        !self.inner.lock().done_list.is_empty()
    }

    pub fn is_error(&self) -> bool {
        self.inner.lock().error
    }

    /// 流是否处于 STREAMON 状态（DQBUF 阻塞等待条件之一：停流后
    /// 返回 EINVAL 而非继续等待——对齐 Linux `!q->streaming → -EINVAL`）。
    pub fn is_streaming(&self) -> bool {
        self.inner.lock().streaming
    }

    /// 置队列错误并唤醒等待者（任务上下文安全）。
    pub fn set_error(&self) {
        self.inner.lock().error = true;
        self.poll_rx.wake_from_irq(IoEvents::ERR);
    }
    // ── 查询 ───────────────────────────────────────────────────────

    /// 取第一个 Active（已排队给驱动）缓冲的索引作为采集目标；无则 None。
    /// 驱动在拼帧目标耗尽时调用（uvc acquire_dest）；vivid 的批量填充
    /// 遍历所有 Active，不走此单取 API。
    pub fn take_active(&self) -> Option<u32> {
        self.inner
            .lock()
            .buffers
            .iter()
            .position(|b| b.state == BufferState::Active)
            .map(|i| i as u32)
    }

    /// 已 qbuf 未 dqbuf 的缓冲数（Queued + Active——qbuf 后不摘除直到
    /// dqbuf；含驱动拼帧中的。对齐 Linux `vb2_queue::queued_count`）。
    pub fn queued_count(&self) -> u32 {
        self.inner.lock().queued_list.len() as u32
    }
    // ── 内存访问 ───────────────────────────────────────────────────
}

/// IoctlOps 委托的 API——驱动 ioctl 处理（reqbufs/querybuf/qbuf/dqbuf/streamon/streamoff）直接调用的队列操作。
impl<M: Vb2MemOps> Vb2Queue<M> {
    // ── 分配 ──────────────────────────────────────────────────

    pub fn reqbufs(&self, count: u32, plane_sizes: &[u32]) -> Result<(), V4l2Error> {
        let mut inner = self.inner.lock();
        if inner.streaming {
            return Err(V4l2Error::Busy);
        }

        // 释放之前的 buffer（allocator 清理）。
        for vb in inner.buffers.drain(..) {
            self.alloc.release(&vb.planes);
        }
        inner.queued_list.clear();
        inner.done_list.clear();
        inner.owned_by_drv_count = 0;
        inner.sequence = 0;

        if count == 0 {
            return Ok(());
        }

        let num_buffers = count.clamp(self.min_buffers, self.max_buffers);
        if plane_sizes.is_empty() || plane_sizes.len() > VIDEO_MAX_PLANES {
            return Err(V4l2Error::InvalidArgument);
        }

        for i in 0..num_buffers {
            let planes = self.alloc.alloc(plane_sizes)?;
            let vb = Vb2Buffer {
                index: i,
                state: BufferState::Dequeued,
                planes,
                data_offset: [0; VIDEO_MAX_PLANES],
                bytesused: 0,
                sequence: 0,
                timestamp: 0,
                timestamp_flags: 0,
                field: 0,
                prepared: false,
            };
            inner.buffers.push(vb);
        }

        Ok(())
    }
    // ── Buffer 操作 ───────────────────────────────────────────────

    pub fn qbuf(&self, index: u32) -> Result<(), V4l2Error> {
        let mut inner = self.inner.lock();
        if inner.error {
            return Err(V4l2Error::Io);
        }

        let vb = inner
            .buffers
            .get_mut(index as usize)
            .ok_or(V4l2Error::InvalidArgument)?;

        QueueInner::__prepare(vb)?;

        vb.state = BufferState::Queued;
        inner.queued_list.push_back(index);

        if inner.streaming && !inner.error {
            if inner.start_streaming_called {
                inner.__enqueue_in_driver(index);
            } else {
                inner.__start_streaming_now();
            }
        }

        Ok(())
    }

    pub fn dqbuf(&self) -> Result<u32, V4l2Error> {
        let mut inner = self.inner.lock();
        let idx = inner.done_list.pop_front().ok_or(V4l2Error::Busy)?;

        let vb = &mut inner.buffers[idx as usize];
        if vb.state != BufferState::Done && vb.state != BufferState::Error {
            return Err(V4l2Error::InvalidArgument);
        }

        vb.prepared = false;
        vb.state = BufferState::Dequeued;

        if let Some(pos) = inner.queued_list.iter().position(|&i| i == idx) {
            inner.queued_list.remove(pos);
        }

        Ok(idx)
    }

    pub fn prepare_buf(&self, index: u32) -> Result<(), V4l2Error> {
        let mut inner = self.inner.lock();
        if inner.error {
            return Err(V4l2Error::Io);
        }

        let vb = inner
            .buffers
            .get_mut(index as usize)
            .ok_or(V4l2Error::InvalidArgument)?;

        QueueInner::__prepare(vb)?;
        Ok(())
    }
    // ── 流控制 ────────────────────────────────────────────────────

    pub fn streamon(&self) -> Result<(), V4l2Error> {
        let mut inner = self.inner.lock();
        if inner.streaming {
            return Err(V4l2Error::Busy);
        }
        if inner.buffers.is_empty() {
            return Err(V4l2Error::InvalidArgument);
        }

        inner.streaming = true;
        inner.__start_streaming_now();

        Ok(())
    }

    pub fn streamoff(&self) {
        let mut inner = self.inner.lock();
        for i in 0..inner.buffers.len() {
            if inner.buffers[i].state == BufferState::Active {
                log::debug!(
                    "vb2: buffer {} still active after stop_streaming, forcing Error",
                    i
                );
                inner.buffers[i].bytesused = 0;
                inner.buffers[i].field = 0;
                inner.buffers[i].state = BufferState::Error;
                inner.done_list.push_back(i as u32);
                if inner.owned_by_drv_count > 0 {
                    inner.owned_by_drv_count -= 1;
                }
            }
        }

        for vb in &mut inner.buffers {
            if vb.state != BufferState::Dequeued {
                vb.state = BufferState::Dequeued;
                vb.prepared = false;
            }
        }

        inner.streaming = false;
        inner.start_streaming_called = false;
        inner.error = false;
        inner.queued_list.clear();
        inner.done_list.clear();
        inner.owned_by_drv_count = 0;
        drop(inner);
        // 状态发布后唤醒全部等待者（对齐 Linux `__vb2_queue_cancel` 末尾的
        // `wake_up_all(&q->done_wq)`）：阻塞的 DQBUF 重查 `!streaming` 返回
        // EINVAL，poll 等待者重查电平——否则停流后等待者永久睡眠。
        self.poll_rx.wake_from_irq(IoEvents::IN | IoEvents::ERR);
    }
    // ── 查询（querybuf/reqbufs 支撑）──────────────────────────────

    /// buffer 元数据的快照。返回前会释放队列锁，
    /// 因此快照可以跨调用安全保存。
    pub fn buffer_snapshot(&self, index: u32) -> Option<Vb2Buffer> {
        self.inner.lock().buffers.get(index as usize).cloned()
    }

    /// 当前分配的缓冲总数（REQBUFS 协商后的实际数量）。
    pub fn num_buffers(&self) -> u32 {
        self.inner.lock().buffers.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec};
    use core::task::{Context, RawWaker, RawWakerVTable, Waker};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axpoll::IoEvents;

    use super::*;

    /// 测试内存分配器：Vec 支撑的平面（host 测试无需真实页分配器）。
    #[derive(Default)]
    struct TestAlloc {
        storage: std::sync::Mutex<Vec<Vec<u8>>>,
    }

    impl Vb2MemOps for TestAlloc {
        fn alloc(&self, sizes: &[u32]) -> Result<Vec<MemPlane>, v4l2_core::V4l2Error> {
            let mut storage = self.storage.lock().unwrap();
            let mut planes = Vec::new();
            for &size in sizes {
                let buf = vec![0u8; size as usize];
                let va = buf.as_ptr() as usize;
                storage.push(buf);
                planes.push(MemPlane {
                    cookie: va,
                    offset: 0,
                    length: size,
                });
            }
            Ok(planes)
        }

        fn release(&self, _planes: &[MemPlane]) {}

        fn mmap(&self, plane: &MemPlane) -> Vec<usize> {
            (0..plane.length.div_ceil(4096))
                .map(|i| plane.cookie + i as usize * 4096)
                .collect()
        }
    }

    /// 计数 waker：每次 wake 递增（用于断言完成路径确实唤醒了等待者）。
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop_fn);

    fn clone(data: *const ()) -> RawWaker {
        let arc = unsafe { Arc::from_raw(data as *const AtomicUsize) };
        let cloned = Arc::clone(&arc);
        core::mem::forget(arc);
        RawWaker::new(Arc::into_raw(cloned).cast(), &VTABLE)
    }

    unsafe fn wake(data: *const ()) {
        unsafe {
            Arc::from_raw(data as *const AtomicUsize).fetch_add(1, Ordering::SeqCst);
        }
    }

    unsafe fn wake_by_ref(data: *const ()) {
        unsafe {
            let arc = Arc::from_raw(data as *const AtomicUsize);
            arc.fetch_add(1, Ordering::SeqCst);
            core::mem::forget(arc);
        }
    }

    unsafe fn drop_fn(data: *const ()) {
        drop(unsafe { Arc::from_raw(data as *const AtomicUsize) });
    }

    fn counting_waker() -> (Waker, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let raw = RawWaker::new(Arc::into_raw(Arc::clone(&count)).cast(), &VTABLE);
        // SAFETY: raw 指针来自本函数构造的 Arc<AtomicUsize>，vtable 全套实现。
        let waker = unsafe { Waker::from_raw(raw) };
        (waker, count)
    }

    fn streaming_queue() -> Vb2Queue<TestAlloc> {
        let q = Vb2Queue::new(TestAlloc::default(), 1, 4);
        q.reqbufs(2, &[4096]).unwrap();
        q.qbuf(0).unwrap();
        q.streamon().unwrap();
        q
    }

    /// buffer_done 发布就绪后必须唤醒注册了 IN 的等待者，
    /// 且 is_readable 变为 true（poll 电平/边沿一致性）。
    #[test]
    fn buffer_done_wakes_poll_in() {
        let q = streaming_queue();
        let (waker, count) = counting_waker();
        let _cx = Context::from_waker(&waker);
        // SAFETY: host 测试线程等价任务上下文，满足 PollSet::register 约束。
        unsafe { q.poll_set().register(&waker, IoEvents::IN) };
        assert_eq!(count.load(Ordering::SeqCst), 0);

        q.buffer_done(0, BufferState::Done, 4096, 0).unwrap();

        assert!(q.is_readable());
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    /// set_error 必须唤醒注册了 ERR 的等待者；只注册 IN 的等待者不被唤醒
    /// （兴趣掩码过滤——IN 等待者重查 is_readable 仍为 false）。
    #[test]
    fn set_error_wakes_only_err_interest() {
        let q = streaming_queue();
        let (waker_err, err_count) = counting_waker();
        let (waker_in, in_count) = counting_waker();
        // SAFETY: host 测试线程等价任务上下文。
        unsafe {
            q.poll_set().register(&waker_err, IoEvents::ERR);
            q.poll_set().register(&waker_in, IoEvents::IN);
        }

        q.set_error();

        assert!(q.is_error());
        assert_eq!(err_count.load(Ordering::SeqCst), 1);
        assert_eq!(in_count.load(Ordering::SeqCst), 0);
    }

    /// buffer_done 拒绝非法转换（非 Active）时不产生唤醒。
    #[test]
    fn rejected_buffer_done_does_not_wake() {
        let q = streaming_queue();
        let (waker, count) = counting_waker();
        // SAFETY: host 测试线程等价任务上下文。
        unsafe { q.poll_set().register(&waker, IoEvents::IN) };

        // 索引越界 → 拒绝，无唤醒。
        assert!(q.buffer_done(9, BufferState::Done, 0, 0).is_err());
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    /// 回归：STREAMOFF 必须唤醒等待者——否则阻塞的 DQBUF 在停流后
    /// 永久睡眠（等待条件 readable/error 永假，无后续唤醒源）。
    /// 对齐 Linux `__vb2_queue_cancel` 末尾的 `wake_up_all(&q->done_wq)`：
    /// 唤醒后等待者重查 `!streaming` 即返回 EINVAL。
    #[test]
    fn streamoff_wakes_waiters() {
        let q = streaming_queue();
        let (waker, count) = counting_waker();
        // SAFETY: host 测试线程等价任务上下文。
        unsafe { q.poll_set().register(&waker, IoEvents::IN | IoEvents::ERR) };
        assert_eq!(count.load(Ordering::SeqCst), 0);

        q.streamoff();

        assert_eq!(count.load(Ordering::SeqCst), 1, "STREAMOFF 必须唤醒等待者");
        assert!(!q.is_readable());
    }

    /// is_streaming 必须如实反映 STREAMON/STREAMOFF 翻转
    /// （DQBUF 等待条件的组成部分）。
    #[test]
    fn is_streaming_reflects_streamon_and_streamoff() {
        let q = Vb2Queue::new(TestAlloc::default(), 1, 4);
        q.reqbufs(2, &[4096]).unwrap();
        assert!(!q.is_streaming(), "REQBUFS 后未 STREAMON");

        q.qbuf(0).unwrap();
        q.streamon().unwrap();
        assert!(q.is_streaming());

        q.streamoff();
        assert!(!q.is_streaming());
    }
}
