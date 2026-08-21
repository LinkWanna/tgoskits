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
}

impl CaptureSession {
    pub(crate) fn new() -> Self {
        Self {
            parser: FrameParser::new(),
            dest: None,
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
    // 无 Active 缓冲（队列满）：本批数据注定丢弃，直接返回（省处理时间；
    // 恢复后由帧边界事件重新获取目标、FID 自然重同步）。
    if dest.is_none() {
        return;
    }
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
                }
                // 帧完成（空帧不发事件——对齐 Linux）：换目标缓冲；无则丢弃。
                dest = acquire_dest(queue);
                if dest.is_none() {
                    break;
                }
            }
            if result.retry {
                // 新帧第一包：换目标缓冲后重调同一包（对齐 -EAGAIN 循环）。
                out = dest_out(dest);
                result = session.parser.push_packet(pkt, out);
                continue;
            }
            break;
        }
        if dest.is_none() {
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
