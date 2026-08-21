//! UVC 视频流 — 基于 ISO 批请求的帧采集（流水线模型）。
//!
//! STREAMON 时 spawn 流 worker：`IsoBatchPipeline` 预填充 `ISO_DEPTH` 个
//! 等长槽批到控制器环（dwc2 环容量内），完成一批结算一批并立即回填，
//! 形成持续在飞的流水线；STREAMOFF 时 cancel 全部在飞批 + join。

use alloc::{sync::Arc, vec, vec::Vec};
use core::task::{Context, Poll};

use crab_usb::usb_if::err::USBError;
use v4l2_core::interface::Field;
use videobuffer::{BufferState, Vb2MemOps, Vb2Queue};

use crate::frame::FrameParser;

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
}

impl CaptureSession {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self {
            parser: FrameParser::new(),
            dest: None,
            expected_bytes: None,
        }
    }

    pub(crate) fn with_expected(expected: Option<usize>) -> Self {
        Self {
            parser: FrameParser::new(),
            dest: None,
            expected_bytes: expected,
        }
    }
}

/// 拼帧一批完成的 ISO 数据（任务上下文——流 worker 每批调用一次）。
pub(crate) fn process_iso_batch<M: Vb2MemOps>(
    session: &mut CaptureSession,
    queue: &Vb2Queue<M>,
    data: &[u8],
    actuals: &[usize],
    slot_len: usize,
) {
    // 帧目标跨批保持：仅无在飞目标（帧起点）时取新缓冲。批开头重取会让
    // requeue 的低索引缓冲劫持在飞帧（见 CaptureSession 文档）。
    if session.dest.is_none() {
        session.dest = acquire_dest(queue);
    }
    let mut dest = session.dest;
    // 有目标直写 Active 缓冲；无目标传空切片 → parser 只跟踪头/边界，
    // payload 整帧丢弃。空目标时仍需逐包跟踪 FID/EOF 以保持帧同步，
    // 否则饥饿期间翻转丢失会导致下一帧以错误边界截断（连续取帧
    // 614400/20312/2048 的根因）。
    for (i, &actual) in actuals.iter().enumerate() {
        if actual < 2 {
            continue;
        }
        // 等长槽切分批缓冲（完成时数据已拷回）。
        let pkt = &data[i * slot_len..i * slot_len + actual];
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
                        }
                        // 帧完成（空帧不发事件——对齐 Linux）：换目标缓冲；无则以
                        // 空切片继续跟踪头/边界，payload 整帧丢弃直到重获缓冲。
                        dest = acquire_dest(queue);
                    }
                } else {
                    // 空帧不发事件但仍换目标（对齐 Linux EOF && bytesused==0 分支）。
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

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use std::sync::Mutex as StdMutex;

    use v4l2_core::V4l2Error;
    use videobuffer::{Vb2MemOps, Vb2Queue, buf::MemPlane};

    use super::*;
    use crate::descriptors::PayloadHeaderFlags as Flags;

    struct TestAlloc {
        storage: StdMutex<Vec<Vec<u8>>>,
    }

    impl Default for TestAlloc {
        fn default() -> Self {
            Self {
                storage: StdMutex::new(Vec::new()),
            }
        }
    }

    impl Vb2MemOps for TestAlloc {
        fn alloc(&self, sizes: &[u32]) -> Result<Vec<MemPlane>, V4l2Error> {
            let mut storage = self.storage.lock().unwrap();
            let mut planes = Vec::new();
            for &size in sizes {
                let buf = vec![0u8; size as usize];
                let va = buf.as_ptr() as usize;
                let offset = storage.len() * 4096;
                storage.push(buf);
                planes.push(MemPlane {
                    cookie: va,
                    offset,
                    length: size,
                });
            }
            Ok(planes)
        }

        fn release(&self, _planes: &[MemPlane]) {}

        fn mmap(&self, plane: &MemPlane) -> Vec<usize> {
            let n = plane.length.div_ceil(4096) as usize;
            (0..n).map(|i| plane.cookie + i * 4096).collect()
        }
    }

    const FID0: u8 = 0;
    const FID1: u8 = Flags::FID.bits();
    const EOF: u8 = Flags::EOF.bits();

    fn pkt(flags: u8, payload_len: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(2 + payload_len);
        v.push(2);
        v.push(flags);
        v.extend(core::iter::repeat(0xAA).take(payload_len));
        v
    }

    fn build_batch(packets: &[Vec<u8>], slot_len: usize) -> (Vec<u8>, Vec<usize>) {
        let mut data = vec![0u8; slot_len * packets.len()];
        let mut actuals = Vec::with_capacity(packets.len());
        for (i, p) in packets.iter().enumerate() {
            let actual = p.len();
            data[i * slot_len..i * slot_len + actual].copy_from_slice(p);
            actuals.push(actual);
        }
        (data, actuals)
    }

    #[test]
    fn starvation_keeps_sync_and_drops_truncated() {
        // 期望固定帧 100B，用于复现连续取帧 614400/20312 截断的根因：
        // 队列饥饿期间整批丢弃若不跟踪头会丢失 FID 翻转，导致下一帧以错误边界截断。
        let q = Vb2Queue::new(TestAlloc::default(), 1, 4);
        q.reqbufs(1, &[100]).unwrap();
        q.qbuf(0).unwrap();
        q.streamon().unwrap();

        let mut session = CaptureSession::with_expected(Some(100));
        let slot_len = 32;

        // 同步：先送 FID0 再送 FID1 帧，确保 parser 进入 synced 状态。
        // Frame1: FID1 10 包 *10B =100B，末包 EOF。
        let mut frame1_pkts = Vec::new();
        frame1_pkts.push(pkt(FID0, 1)); // sync 丢弃
        for i in 0..10 {
            let eof = i == 9;
            frame1_pkts.push(pkt(FID1 | if eof { EOF } else { 0 }, 10));
        }
        let (data, actuals) = build_batch(&frame1_pkts, slot_len);
        process_iso_batch(&mut session, &q, &data, &actuals, slot_len);
        assert!(q.is_readable(), "首帧应完整交付");
        let idx = q.dqbuf().unwrap();
        let vb = q.buffer_snapshot(idx).unwrap();
        assert_eq!(vb.bytesused, 100);
        // 不立即 qbuf，制造下次批处理时 Active=0 的饥饿窗口。
        assert!(q.take_active().is_none());

        // 饥饿批：下一帧 FID0 的前半部分 5 包 *10B =50B，无 EOF，dest=None 时仅跟踪头。
        let mut starve_pkts = Vec::new();
        for _ in 0..5 {
            starve_pkts.push(pkt(FID0, 10));
        }
        let (sdata, sactuals) = build_batch(&starve_pkts, slot_len);
        process_iso_batch(&mut session, &q, &sdata, &sactuals, slot_len);
        assert!(!q.is_readable(), "饥饿批不应产出帧");
        // 此时 parser 已跟踪到 FID0，filled 因空目标保持 0。

        // 恢复缓冲
        q.qbuf(idx).unwrap();
        assert!(q.take_active().is_some());

        // 截断帧的后半：剩余 5 包 *10B 并 EOF，累计仅 50B（前半 50B 已因无缓冲丢弃），
        // 固定大小校验应丢弃该截断帧。
        let mut trunc_pkts = Vec::new();
        for i in 0..5 {
            let eof = i == 4;
            trunc_pkts.push(pkt(FID0 | if eof { EOF } else { 0 }, 10));
        }
        let (tdata, tactuals) = build_batch(&trunc_pkts, slot_len);
        process_iso_batch(&mut session, &q, &tdata, &tactuals, slot_len);
        assert!(!q.is_readable(), "截断帧 50B != 期望 100B 应被丢弃");

        // 下一完整帧 FID1 100B 应正常交付，证明同步未丢失且截断被过滤。
        let mut frame3_pkts = Vec::new();
        for i in 0..10 {
            let eof = i == 9;
            frame3_pkts.push(pkt(FID1 | if eof { EOF } else { 0 }, 10));
        }
        let (f3data, f3actuals) = build_batch(&frame3_pkts, slot_len);
        process_iso_batch(&mut session, &q, &f3data, &f3actuals, slot_len);
        assert!(q.is_readable(), "下一完整帧应交付");
        let idx2 = q.dqbuf().unwrap();
        let vb2 = q.buffer_snapshot(idx2).unwrap();
        assert_eq!(vb2.bytesused, 100);
    }

    #[test]
    fn fixed_size_validation_drops_truncated_while_compressed_allows_variable() {
        let q = Vb2Queue::new(TestAlloc::default(), 1, 4);
        q.reqbufs(1, &[100]).unwrap();
        q.qbuf(0).unwrap();
        q.streamon().unwrap();

        // 未压缩固定 100B：50B 截断应丢弃
        let mut sess_fixed = CaptureSession::with_expected(Some(100));
        // 先同步
        process_iso_batch(
            &mut sess_fixed,
            &q,
            &build_batch(&[pkt(FID0, 1)], 32).0,
            &build_batch(&[pkt(FID0, 1)], 32).1,
            32,
        );
        let mut trunc = Vec::new();
        for i in 0..5 {
            let eof = i == 4;
            trunc.push(pkt(FID1 | if eof { EOF } else { 0 }, 10));
        }
        let (data, actuals) = build_batch(&trunc, 32);
        // 需要再送一次同步后的首帧：先送 FID0 同步，再送 FID1 截断
        process_iso_batch(&mut sess_fixed, &q, &data, &actuals, 32);
        // 上述直接以 FID1 起始但 last_fid 仍为 FID0（来自同步包），会累 50B 后 EOF 触发，
        // 因 50 !=100 被丢弃
        assert!(!q.is_readable());

        // 压缩可变大小：同样 50B 应交付
        let q2 = Vb2Queue::new(TestAlloc::default(), 1, 4);
        q2.reqbufs(1, &[100]).unwrap();
        q2.qbuf(0).unwrap();
        q2.streamon().unwrap();
        let mut sess_var = CaptureSession::with_expected(None);
        process_iso_batch(
            &mut sess_var,
            &q2,
            &build_batch(&[pkt(FID0, 1)], 32).0,
            &build_batch(&[pkt(FID0, 1)], 32).1,
            32,
        );
        let (data2, actuals2) = build_batch(&trunc, 32);
        process_iso_batch(&mut sess_var, &q2, &data2, &actuals2, 32);
        assert!(q2.is_readable());
        let idx = q2.dqbuf().unwrap();
        assert_eq!(q2.buffer_snapshot(idx).unwrap().bytesused, 50);
    }
}
