#![allow(dead_code)]
//! UVC 视频流 — 基于 ISO 批请求的帧采集（流水线模型）。
//!
//! STREAMON 时 spawn 流 worker：`IsoBatchPipeline` 预填充 `ISO_DEPTH` 个
//! 等长槽批到控制器环（dwc2 环容量内），完成一批结算一批并立即回填，
//! 形成持续在飞的流水线；STREAMOFF 时 cancel 全部在飞批 + join。

use alloc::{sync::Arc, vec, vec::Vec};
use core::task::{Context, Poll};

use ax_sync::SpinLock;
use crab_usb::usb_if::err::USBError;
use v4l2_core::interface::Field;
use videobuffer::{BufferState, Vb2MemOps, Vb2Queue};

use crate::{frame::FrameParser, stats::UvcStats};

/// 每批微帧数（64 µframes = 8ms/批；与 dwc2 ring 网格对齐）。
pub(crate) const ISO_BATCH: usize = 64;
pub(crate) const ISO_DEPTH: usize = 3;

/// 一批 ISO 传输的完成结果：实际总字节数 + 逐包实际字节数（等长槽布局，
/// 槽 i 数据 = `batch_buf[i * slot_len .. i * slot_len + actuals[i]]`）。
#[derive(Debug, Clone, Default)]
pub struct IsoBatchResult {
    pub total: usize,
    pub actuals: Vec<usize>,
}

/// 在飞 ISO 批句柄：`submit_iso_batch` 提交后由流 worker 轮询。
///
/// 由 OS 侧（UsbDeviceLease::submit_endpoint_transfer 的封装）实现：
/// `poll` 在任务上下文调用（内部注册 waker，硬件完成事件唤醒），
/// `cancel` 停掉在飞请求并唤醒等待者。同一句柄至多轮询一次完成。
pub trait IsoStreamHandle: Send + Sync {
    fn poll(&self, cx: &mut Context<'_>) -> Poll<Result<IsoBatchResult, USBError>>;

    fn cancel(&self) -> Result<(), USBError>;
}

/// 跨批捕获会话：帧解析器 + 当前帧的目标缓冲。
pub(crate) struct CaptureSession {
    pub(crate) parser: FrameParser,
    pub(crate) dest: Option<(u32, usize, usize)>,
    /// 固定帧大小（未压缩格式）用于截断帧过滤；压缩格式为 None（大小可变）。
    pub(crate) expected_bytes: Option<usize>,
    pub(crate) stats: Arc<UvcStats>,
}

impl CaptureSession {
    pub(crate) fn with_expected_and_stats(expected: Option<usize>, stats: Arc<UvcStats>) -> Self {
        Self {
            parser: FrameParser::new(),
            dest: None,
            expected_bytes: expected,
            stats,
        }
    }
}

/// `queue` 与 `session` 均为 `SpinLock` 保护的 IRQ 安全结构。
/// `handle_irq_batch` 在 DWC2 硬中断直接调用，完成拼帧与 `buffer_done`，
/// 之后同一 `slot` 由 DWC2 零分配重入环，无需任务调度。
pub(crate) struct IrqCaptureContext<M: Vb2MemOps> {
    pub(crate) queue: Arc<Vb2Queue<M>>,
    pub(crate) session: SpinLock<CaptureSession>,
    pub(crate) slot_len: usize,
}

impl<M: Vb2MemOps> IrqCaptureContext<M> {
    pub(crate) fn new(
        queue: Arc<Vb2Queue<M>>,
        expected: Option<usize>,
        stats: Arc<UvcStats>,
        slot_len: usize,
    ) -> Self {
        Self {
            queue,
            session: SpinLock::new(CaptureSession::with_expected_and_stats(expected, stats)),
            slot_len,
        }
    }

    /// 硬中断上下文拼帧：无分配、无可睡锁、无格式化。
    ///
    /// # Safety
    /// 仅在 DWC2 硬中断且持有必要 `SpinLockIrqSave` 期间调用。
    pub(crate) fn handle_irq_batch(&self, data: &[u8], actuals: &[usize], slot_len: usize) {
        // 复用 process_iso_batch 核心循环，但通过 SpinLock 获取 session，
        // 且截断帧警告仅计 stats 不分配。
        let mut session = self.session.lock_irqsave();
        process_iso_batch_irq(&mut session, &self.queue, data, actuals, slot_len);
    }
}

/// 硬中断版拼帧（`process_iso_batch` 的零分配变体）
///
/// 与任务版语义一致，但 `log::warn!` 格式化分配被省略，仅计 `truncated` 统计，
/// 确保硬中断无分配。
fn process_iso_batch_irq<M: Vb2MemOps>(
    session: &mut CaptureSession,
    queue: &Vb2Queue<M>,
    data: &[u8],
    actuals: &[usize],
    slot_len: usize,
) {
    use crate::frame::UvcPayloadHeader;

    session.stats.record_batch(actuals.len());
    let mut starved_payload_bytes = 0usize;
    let mut has_starved_batch = false;

    if session.dest.is_none() {
        session.dest = acquire_dest(queue);
        if session.dest.is_none() {
            has_starved_batch = true;
        }
    }
    let mut dest = session.dest;
    if dest.is_none() {
        has_starved_batch = true;
    }
    if has_starved_batch {
        session.stats.record_batch_starved();
    }
    for (i, &actual) in actuals.iter().enumerate() {
        if actual == 0 {
            continue;
        }
        if actual > 0 {
            session.stats.record_packet_with_data(actual);
        }
        if actual < 2 {
            session.stats.record_invalid_header();
            continue;
        }
        let pkt = &data[i * slot_len..i * slot_len + actual];
        let hdr_opt = UvcPayloadHeader::parse(pkt);
        if hdr_opt.is_none() {
            session.stats.record_invalid_header();
            continue;
        }
        let (hdr, hdr_len) = hdr_opt.unwrap();
        if hdr.has_err {
            session.stats.record_err_packet();
        }
        let payload_len = pkt.len().saturating_sub(hdr_len);
        if dest.is_none() && payload_len > 0 {
            starved_payload_bytes += payload_len;
        } else if payload_len > 0 {
            session.stats.record_payload(payload_len);
        }
        let mut out: &mut [u8] = dest_out(dest);
        let mut result = session.parser.push_packet(pkt, out);
        loop {
            if let Some(evt) = result.evt {
                if evt.bytes > 0 {
                    let valid = session.expected_bytes.is_none_or(|exp| evt.bytes == exp);
                    if !valid {
                        // 硬中断内不做格式化 warn，仅计统计。
                        session.stats.record_frame_dropped_truncated();
                    } else {
                        if let Some((idx, ..)) = dest.take() {
                            let _ = queue.buffer_done(
                                idx,
                                BufferState::Done,
                                evt.bytes as u32,
                                Field::NoField as u32,
                            );
                            session.stats.record_frame_done();
                        } else {
                            session.stats.record_frame_dropped_truncated();
                        }
                        dest = acquire_dest(queue);
                    }
                } else {
                    session.stats.record_frame_dropped_empty();
                    dest = acquire_dest(queue);
                }
            }
            if result.retry {
                out = dest_out(dest);
                result = session.parser.push_packet(pkt, out);
                continue;
            }
            break;
        }
    }
    if starved_payload_bytes > 0 {
        session
            .stats
            .record_bytes_dropped_starved(starved_payload_bytes);
    }
    session.dest = dest;
}

/// 拼帧一批完成的 ISO 数据（任务上下文——流 worker 每批调用一次）。
pub(crate) fn process_iso_batch<M: Vb2MemOps>(
    session: &mut CaptureSession,
    queue: &Vb2Queue<M>,
    data: &[u8],
    actuals: &[usize],
    slot_len: usize,
) {
    use crate::frame::UvcPayloadHeader;

    // 诊断：批维度计数
    session.stats.record_batch(actuals.len());
    let mut starved_payload_bytes = 0usize;
    let mut has_starved_batch = false;

    // 帧目标跨批保持：仅无在飞目标（帧起点）时取新缓冲。批开头重取会让
    // requeue 的低索引缓冲劫持在飞帧（见 CaptureSession 文档）。
    if session.dest.is_none() {
        session.dest = acquire_dest(queue);
        if session.dest.is_none() {
            has_starved_batch = true;
        }
    }
    let mut dest = session.dest;
    if dest.is_none() {
        has_starved_batch = true;
    }
    if has_starved_batch {
        session.stats.record_batch_starved();
    }
    // 有目标直写 Active 缓冲；无目标传空切片 → parser 只跟踪头/边界，
    // payload 整帧丢弃。空目标时仍需逐包跟踪 FID/EOF 以保持帧同步，
    // 否则饥饿期间翻转丢失会导致下一帧以错误边界截断（连续取帧
    // 614400/20312/2048 的根因）。
    for (i, &actual) in actuals.iter().enumerate() {
        if actual == 0 {
            continue;
        }
        if actual > 0 {
            session.stats.record_packet_with_data(actual);
        }
        if actual < 2 {
            session.stats.record_invalid_header();
            continue;
        }
        // 等长槽切分批缓冲（完成时数据已拷回）。
        let pkt = &data[i * slot_len..i * slot_len + actual];
        // 预解析头用于统计（不影响 frame.rs 的权威解析）
        let hdr_opt = UvcPayloadHeader::parse(pkt);
        if hdr_opt.is_none() {
            session.stats.record_invalid_header();
            continue;
        }
        let (hdr, hdr_len) = hdr_opt.unwrap();
        if hdr.has_err {
            session.stats.record_err_packet();
        }
        let payload_len = pkt.len().saturating_sub(hdr_len);
        if dest.is_none() && payload_len > 0 {
            starved_payload_bytes += payload_len;
        } else if payload_len > 0 {
            session.stats.record_payload(payload_len);
        }
        // 有目标直写 Active 缓冲；无目标传空切片 → parser 只跟踪头/边界，
        // payload 整帧丢弃。
        let mut out: &mut [u8] = dest_out(dest);
        // Linux do-while（uvc_video.c）：FID 翻转完成帧 → done + 换目标 →
        // 同一包重调（retry）；新帧第一包 payload 从不丢弃。
        let mut result = session.parser.push_packet(pkt, out);
        loop {
            if let Some(evt) = result.evt {
                if evt.bytes > 0 {
                    let valid = session.expected_bytes.is_none_or(|exp| evt.bytes == exp);
                    if !valid {
                        log::warn!(
                            "[UVC] drop truncated frame: got {} expected {:?}",
                            evt.bytes,
                            session.expected_bytes
                        );
                        session.stats.record_frame_dropped_truncated();
                        // 复用同一 Active 缓冲承载下一帧（filled 已在 finish_frame 清零，
                        // 下一包将从 0 覆盖），避免截断帧被当成正常帧交付。
                    } else {
                        // 完整帧：入 vb2 done 队列（内建唤醒：DQBUF 阻塞与
                        // poll 共用队列 vb_poll_set——无需在此手动通知）。
                        if let Some((idx, ..)) = dest.take() {
                            // buffer_done 失败 = idx 非 Active（与 take_active 竞争，
                            // 正常不可达）；不做恢复，仅显式忽略。
                            let _ = queue.buffer_done(
                                idx,
                                BufferState::Done,
                                evt.bytes as u32,
                                Field::NoField as u32,
                            );
                            session.stats.record_frame_done();
                        } else {
                            // 无目标时产生的完整帧（理论上仅空目标跟踪时 filled==0，
                            // 不应出现 bytes>0），计为丢弃
                            session.stats.record_frame_dropped_truncated();
                        }
                        // 帧完成（空帧不发事件——对齐 Linux）：换目标缓冲；无则以
                        // 空切片继续跟踪头/边界，payload 整帧丢弃直到重获缓冲。
                        dest = acquire_dest(queue);
                    }
                } else {
                    // 空帧不发事件但仍换目标（对齐 Linux EOF && bytesused==0 分支）。
                    session.stats.record_frame_dropped_empty();
                    dest = acquire_dest(queue);
                }
            }
            if result.retry {
                // 新帧第一包：换目标缓冲后重调同一包（对齐 -EAGAIN 循环）。
                // 无目标时以空切片重调，仅跟踪头不写 payload。
                out = dest_out(dest);
                result = session.parser.push_packet(pkt, out);
                continue;
            }
            break;
        }
    }
    if starved_payload_bytes > 0 {
        session
            .stats
            .record_bytes_dropped_starved(starved_payload_bytes);
    }
    session.dest = dest;
}

pub(crate) struct IsoBatchPipeline {
    slot_len: usize,
    lengths: Vec<usize>,
    slots: Vec<IsoBatchSlot>,
}

struct IsoBatchSlot {
    buffer: Vec<u8>,
    handle: Option<Arc<dyn IsoStreamHandle>>,
}

impl IsoBatchPipeline {
    pub(crate) fn new(slot_len: usize, depth: usize) -> Self {
        let lengths = vec![slot_len; ISO_BATCH];
        let slots = (0..depth)
            .map(|_| IsoBatchSlot {
                buffer: vec![0u8; slot_len * ISO_BATCH],
                handle: None,
            })
            .collect();
        Self {
            slot_len,
            lengths,
            slots,
        }
    }

    pub(crate) fn submit_pending(
        &mut self,
        handle: &dyn crate::UvcHandle,
        endpoint: u8,
    ) -> Result<(), USBError> {
        for slot in self.slots.iter_mut() {
            if slot.handle.is_some() {
                continue;
            }
            match handle.submit_iso_batch(endpoint, &mut slot.buffer, &self.lengths) {
                Ok(pending) => slot.handle = Some(pending),
                Err(USBError::SlotLimitReached) => break,
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    pub(crate) fn poll_process<M: Vb2MemOps>(
        &mut self,
        cx: &mut Context<'_>,
        session: &mut CaptureSession,
        queue: &Vb2Queue<M>,
    ) -> Poll<Result<(), USBError>> {
        let mut processed = false;
        for slot in self.slots.iter_mut() {
            let Some(pending) = slot.handle.as_ref() else {
                continue;
            };
            match pending.poll(cx) {
                Poll::Ready(Ok(result)) => {
                    process_iso_batch(session, queue, &slot.buffer, &result.actuals, self.slot_len);
                    slot.handle = None;
                    processed = true;
                }
                Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                Poll::Pending => {}
            }
        }
        if processed {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    pub(crate) fn cancel_all(&self) -> Result<(), USBError> {
        for slot in &self.slots {
            if let Some(pending) = &slot.handle {
                pending.cancel()?;
            }
        }
        Ok(())
    }

    pub(crate) fn in_flight(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.handle.is_some())
            .count()
    }

    pub(crate) fn in_flight_handles(&self) -> Vec<Arc<dyn IsoStreamHandle>> {
        self.slots
            .iter()
            .filter_map(|slot| slot.handle.clone())
            .collect()
    }
}

/// 从拼帧目标（Active 缓冲平面地址 + 长度）构造可变切片；无目标返回空切片。
///
/// # SAFETY
/// `va` 是 vb2 队列 Active 缓冲的平面虚拟地址，`len` 为其分配长度；缓冲在
/// `buffer_done` 前归驱动独占，无别名写。
fn dest_out(dest: Option<(u32, usize, usize)>) -> &'static mut [u8] {
    match dest {
        Some((_, va, len)) => unsafe { core::slice::from_raw_parts_mut(va as *mut u8, len) },
        None => &mut [],
    }
}

/// 取队列中第一个 Active（已排队给驱动）缓冲作为拼帧目标；无则返回 None。
fn acquire_dest<M: Vb2MemOps>(queue: &Vb2Queue<M>) -> Option<(u32, usize, usize)> {
    let idx = queue.take_active()?;
    let plane = queue.buffer_snapshot(idx)?.planes.first()?.clone();
    let va = plane.cookie;
    if va == 0 {
        return None;
    }
    Some((idx, va, plane.length as usize))
}
