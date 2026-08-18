//! 每个 buffer 的元数据——对应 Linux 的 `struct vb2_buffer`。

use alloc::vec::Vec;

/// 每个 buffer 的最大 plane 数（符合 V4L2 规范）。
pub const VIDEO_MAX_PLANES: usize = 8;

/// 队列中单个 buffer 的状态。
///
/// 对应 Linux 的 `enum vb2_buffer_state`：
///
/// ```text
/// DEQUEUED ──(QBUF)──► QUEUED ──(buf_queue)──► ACTIVE
///     ▲                    │                        │
///     │                    │                        │ (vb2_buffer_done)
///     │                    │                        ▼
///     └──(DQBUF)───────────┴────────────────── DONE / ERROR
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferState {
    /// buffer 处于用户空间控制下（之前是 Free）。
    Dequeued,
    /// buffer 已被用户空间入队，等待交给驱动。
    Queued,
    /// buffer 已通过 buf_queue 交给驱动，驱动正在处理它。
    Active,
    /// 驱动已处理完毕，可供 DQBUF。
    Done,
    /// 驱动处理此 buffer 时遇到错误。
    Error,
}

/// buffer 中一个 plane 的内存句柄。
///
/// 由 [`Vb2MemOps::alloc`](super::Vb2MemOps::alloc) 返回的不透明令牌。
/// allocator 实现知道如何从该 cookie 中提取
/// phys_addr / vaddr。
#[derive(Debug, Clone)]
pub struct MemPlane {
    /// 分配器私有句柄——但必须可直接用作 CPU 写地址（vmalloc 风格，
    /// 拼帧/填充 `cookie as *mut u8` 直写）。
    pub cookie: usize,
    /// UAPI mmap 偏移（`v4l2_buffer.m.offset`）——分配器在 `alloc` 时
    /// 按自己的布局（stride）计算填入；队列/glue 只读。
    pub offset: usize,
    /// 平面长度（页对齐）。
    pub length: u32,
}

/// 队列中的单个 buffer——对应 Linux 的 `struct vb2_buffer`。
#[derive(Clone)]
pub struct Vb2Buffer {
    /// buffer 在队列中的索引（0..num_buffers-1）。
    pub index: u32,

    /// 当前状态。
    pub state: BufferState,

    /// 每个 plane 的内存句柄。
    pub planes: Vec<MemPlane>,

    /// 每个 plane 内的数据偏移（用于多平面格式）。
    pub data_offset: [u32; VIDEO_MAX_PLANES],

    /// 此 buffer 中有效数据的字节数（由驱动设置）。
    pub bytesused: u32,

    /// 帧 sequence 号（单调递增，由队列分配）。
    pub sequence: u32,

    /// 时间戳（ns，CLOCK_MONOTONIC）。
    pub timestamp: u64,

    /// 时间戳标志（V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC 等）。
    pub timestamp_flags: u32,

    /// field 类型（NONE / TOP / BOTTOM / INTERLACED 等）。
    pub field: u32,

    // ── 内部标志 ──
    /// buffer 已准备（buf_prepare 已成功调用）。
    pub(crate) prepared: bool,
}
