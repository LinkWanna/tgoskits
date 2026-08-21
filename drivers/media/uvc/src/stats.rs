//! UVC 统计 — 参考 `dwc2/stats.rs` 的快照模型，用于诊断连续取帧丢帧。
//!
//! `UvcStats` 为 `Arc` 共享的原子计数器，worker 任务侧无锁更新，
//! `close_stream` 侧快照并 `info!` 打印。所有计数 `Relaxed` 即可
//! （仅诊断用途，无跨计数器一致性要求）。

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UvcStatsSnapshot {
    /// 已完成批数（每批 `ISO_BATCH=64` 微帧）。
    pub batches: usize,
    /// 总包数（批 *64）。
    pub packets_total: usize,
    /// `actual>0` 的有效包数。
    pub packets_with_data: usize,
    /// `actual>=2` 但头解析失败的包数。
    pub invalid_headers: usize,
    /// 收到的总字节（`actual` 之和，含头）。
    pub bytes_received: usize,
    /// 载荷字节（`actual - header_len`，仅有效头）。
    pub bytes_payload: usize,
    /// 已交付的完整帧数（`buffer_done Done`）。
    pub frames_done: usize,
    /// 因固定大小校验丢弃的截断帧数（`got != expected`）。
    pub frames_dropped_truncated: usize,
    /// 空帧数（`bytes==0` 的 EOF，不交付）。
    pub frames_dropped_empty: usize,
    /// 饥饿批数（批首 `dest is None`）。
    pub batches_starved: usize,
    /// 饥饿期间丢弃的载荷字节。
    pub bytes_dropped_starved: usize,
    /// 携带 ERR 位的包数。
    pub err_packets: usize,
    /// 批间隔：最大 / 平均（ns），用于判断调度是否及时。
    pub batch_interval_max_ns: u64,
    pub batch_interval_avg_ns: u64,
}

#[derive(Default)]
struct UvcStatsInner {
    batches: AtomicUsize,
    packets_total: AtomicUsize,
    packets_with_data: AtomicUsize,
    invalid_headers: AtomicUsize,
    bytes_received: AtomicUsize,
    bytes_payload: AtomicUsize,
    frames_done: AtomicUsize,
    frames_dropped_truncated: AtomicUsize,
    frames_dropped_empty: AtomicUsize,
    batches_starved: AtomicUsize,
    bytes_dropped_starved: AtomicUsize,
    err_packets: AtomicUsize,
    last_batch_ns: AtomicU64,
    batch_interval_sum_ns: AtomicU64,
    batch_interval_cnt: AtomicUsize,
    batch_interval_max_ns: AtomicU64,
}

#[derive(Clone, Default)]
pub struct UvcStats {
    inner: Arc<UvcStatsInner>,
}

impl UvcStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&self) {
        self.inner.batches.store(0, Ordering::Relaxed);
        self.inner.packets_total.store(0, Ordering::Relaxed);
        self.inner.packets_with_data.store(0, Ordering::Relaxed);
        self.inner.invalid_headers.store(0, Ordering::Relaxed);
        self.inner.bytes_received.store(0, Ordering::Relaxed);
        self.inner.bytes_payload.store(0, Ordering::Relaxed);
        self.inner.frames_done.store(0, Ordering::Relaxed);
        self.inner
            .frames_dropped_truncated
            .store(0, Ordering::Relaxed);
        self.inner.frames_dropped_empty.store(0, Ordering::Relaxed);
        self.inner.batches_starved.store(0, Ordering::Relaxed);
        self.inner.bytes_dropped_starved.store(0, Ordering::Relaxed);
        self.inner.err_packets.store(0, Ordering::Relaxed);
        self.inner.last_batch_ns.store(0, Ordering::Relaxed);
        self.inner.batch_interval_sum_ns.store(0, Ordering::Relaxed);
        self.inner.batch_interval_cnt.store(0, Ordering::Relaxed);
        self.inner.batch_interval_max_ns.store(0, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> UvcStatsSnapshot {
        let cnt = self.inner.batch_interval_cnt.load(Ordering::Relaxed);
        let sum = self.inner.batch_interval_sum_ns.load(Ordering::Relaxed);
        let avg = if cnt > 0 { sum / cnt as u64 } else { 0 };
        UvcStatsSnapshot {
            batches: self.inner.batches.load(Ordering::Relaxed),
            packets_total: self.inner.packets_total.load(Ordering::Relaxed),
            packets_with_data: self.inner.packets_with_data.load(Ordering::Relaxed),
            invalid_headers: self.inner.invalid_headers.load(Ordering::Relaxed),
            bytes_received: self.inner.bytes_received.load(Ordering::Relaxed),
            bytes_payload: self.inner.bytes_payload.load(Ordering::Relaxed),
            frames_done: self.inner.frames_done.load(Ordering::Relaxed),
            frames_dropped_truncated: self.inner.frames_dropped_truncated.load(Ordering::Relaxed),
            frames_dropped_empty: self.inner.frames_dropped_empty.load(Ordering::Relaxed),
            batches_starved: self.inner.batches_starved.load(Ordering::Relaxed),
            bytes_dropped_starved: self.inner.bytes_dropped_starved.load(Ordering::Relaxed),
            err_packets: self.inner.err_packets.load(Ordering::Relaxed),
            batch_interval_max_ns: self.inner.batch_interval_max_ns.load(Ordering::Relaxed),
            batch_interval_avg_ns: avg,
        }
    }

    pub(crate) fn record_batch(&self, packets: usize) {
        // 计算批间隔：worker 调度及时性诊断，最大间隔 >>8ms 说明调度被抢占或在飞深度不足
        let now = ax_runtime::hal::time::monotonic_time_nanos();
        let prev = self.inner.last_batch_ns.swap(now, Ordering::Relaxed);
        if prev != 0 {
            let delta = now.saturating_sub(prev);
            self.inner
                .batch_interval_sum_ns
                .fetch_add(delta, Ordering::Relaxed);
            self.inner
                .batch_interval_cnt
                .fetch_add(1, Ordering::Relaxed);
            // 最大值 CAS 更新
            let mut cur = self.inner.batch_interval_max_ns.load(Ordering::Relaxed);
            while delta > cur {
                match self.inner.batch_interval_max_ns.compare_exchange(
                    cur,
                    delta,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(v) => cur = v,
                }
            }
        }
        self.inner.batches.fetch_add(1, Ordering::Relaxed);
        self.inner
            .packets_total
            .fetch_add(packets, Ordering::Relaxed);
    }

    pub(crate) fn record_batch_starved(&self) {
        self.inner.batches_starved.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_packet_with_data(&self, bytes: usize) {
        self.inner.packets_with_data.fetch_add(1, Ordering::Relaxed);
        self.inner
            .bytes_received
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn record_payload(&self, payload: usize) {
        self.inner
            .bytes_payload
            .fetch_add(payload, Ordering::Relaxed);
    }

    pub(crate) fn record_bytes_dropped_starved(&self, payload: usize) {
        self.inner
            .bytes_dropped_starved
            .fetch_add(payload, Ordering::Relaxed);
    }

    pub(crate) fn record_invalid_header(&self) {
        self.inner.invalid_headers.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_err_packet(&self) {
        self.inner.err_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_frame_done(&self) {
        self.inner.frames_done.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_frame_dropped_truncated(&self) {
        self.inner
            .frames_dropped_truncated
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_frame_dropped_empty(&self) {
        self.inner
            .frames_dropped_empty
            .fetch_add(1, Ordering::Relaxed);
    }
}

impl core::fmt::Display for UvcStatsSnapshot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "batches={} packets={}/{} invalid_hdr={} bytes={}/payload={} frames_done={} \
             dropped_trunc={} dropped_empty={} starved_batches={} starved_bytes={} err={} \
             batch_interval_avg={}ms max={}ms",
            self.batches,
            self.packets_with_data,
            self.packets_total,
            self.invalid_headers,
            self.bytes_received,
            self.bytes_payload,
            self.frames_done,
            self.frames_dropped_truncated,
            self.frames_dropped_empty,
            self.batches_starved,
            self.bytes_dropped_starved,
            self.err_packets,
            self.batch_interval_avg_ns / 1_000_000,
            self.batch_interval_max_ns / 1_000_000,
        )
    }
}
