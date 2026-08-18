use crate::descriptors::PayloadHeaderFlags as Flags;

/// UVC 载荷头（2.4.3.3）
#[derive(Debug, Clone, Default)]
pub struct UvcPayloadHeader {
    pub length: u8,              // bLength
    pub info: u8,                // bmHeaderInfo
    pub fid: bool,               // 帧 ID
    pub eof: bool,               // 帧结束
    pub pts: Option<u32>,        // 演示时间戳（4 字节，90kHz）
    pub scr: Option<(u32, u16)>, // 源时钟参考：SOF 时间戳（32 位）+ SOF 计数（16 位）
    pub has_err: bool,
}

impl UvcPayloadHeader {
    /// 从字节流解析 UVC 载荷头；若数据不合法，返回 None 以允许上层丢弃该包。
    pub fn parse(buf: &[u8]) -> Option<(Self, usize)> {
        if buf.len() < 2 {
            return None;
        }
        let b_length = buf[0] as usize;
        let info = buf[1];
        if b_length < 2 || b_length > buf.len() {
            return None;
        }

        let fid = (info & Flags::FID.bits()) != 0;
        let eof = (info & Flags::EOF.bits()) != 0;
        let has_pts = (info & Flags::PTS.bits()) != 0;
        let has_scr = (info & Flags::SCR.bits()) != 0;
        let has_err = (info & Flags::ERR.bits()) != 0;

        // 可选字段顺序：PTS(4) -> SCR(6)
        let mut offset = 2usize;
        let pts = if has_pts {
            if offset + 4 > b_length {
                return None;
            }
            let v = u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]);
            offset += 4;
            Some(v)
        } else {
            None
        };

        let scr = if has_scr {
            if offset + 6 > b_length {
                return None;
            }
            let stc = u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]);
            let sof = u16::from_le_bytes([buf[offset + 4], buf[offset + 5]]);
            Some((stc, sof))
        } else {
            None
        };

        let header = UvcPayloadHeader {
            length: b_length as u8,
            info,
            fid,
            eof,
            pts,
            scr,
            has_err,
        };

        Some((header, b_length))
    }
}

/// 帧组装事件（零拷贝：帧数据直接写入调用者的目标缓冲，这里只携带元数据）
#[derive(Debug, Clone, Copy)]
pub struct FrameEvent {
    /// 写入目标缓冲的帧字节数。
    pub bytes: usize,
    pub pts_90khz: Option<u32>,
    pub eof: bool,
    pub fid: bool,
    pub frame_number: u32,
}

/// 快速结构检查：JPEG 缓冲是否以 SOI 开头？
pub fn is_valid_jpeg(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0xFF && data[1] == 0xD8
}

/// UVC 帧解析/组装状态机（零拷贝）。
///
/// 帧数据由调用者提供目标缓冲（V4L2 mmap buffer），payload 直接写入其中，
/// 解析器不持有、不复制帧数据，出帧时只返回元数据 + 字节数。
///
/// 帧边界语义对齐 Linux uvcvideo（uvc_video.c，唯一权威）：
/// - 帧同步：首次 FID 翻转前丢弃所有数据（对齐 decode_start 的
///   `buf->state != ACTIVE` 同步分支——避免 STREAMON 后加入时抓到半帧）；
/// - 帧完成：FID 翻转（EOF 丢失兜底，:1222-1228）+ EOF（:1397，正常路径，
///   EOF 位比 FID 翻转早一帧）；**完成时本包不消费**（`retry`——对齐
///   `-EAGAIN` + `uvc_video_next_buffers`），调用者换目标缓冲后重调同一包，
///   新帧第一包 payload（含 SOI）从不丢弃；
/// - **不校验内容**：SOI/帧大小完整性交给用户/解码器判断（Linux 仅 quirk
///   路径检查 SOI 边界且只标记 error）；
/// - ERR 包只统计不丢帧（对齐 :1287-1292 的 buf error 标记）。
///
/// 剩余的一次拷贝：microframe payload 从驱动 pool（DMA 直写目标）切片
/// 复制进 vb2 plane（`dest[filled..].copy_from_slice`，见 push_packet ⑤）——
/// plane 是用户可见的 mmap 缓冲，帧边界（FID/EOF）在解析中确定、DMA 无法
/// 预先定位 plane 偏移，此拷贝无法消除（Linux uvcvideo 同款）。路径其余
/// 段零拷贝：USB → pool 为 DMA 直写、pool → parser 为切片引用、plane →
/// 用户空间为 mmap 物理直读。
#[derive(Debug, Default)]
pub struct FrameParser {
    last_fid: Option<bool>,
    last_pts: Option<u32>,
    frame_number: u32,
    error_packet_count: u32,
    /// 非法头丢弃计数
    nb_invalid: u32,
    /// 当前帧已写入目标缓冲的载荷字节数。
    filled: usize,
    /// 是否已通过首次 FID 翻转完成帧同步。
    synced: bool,
}

/// push_packet 结果（对齐 Linux uvc_video_decode_start 的 `-EAGAIN` 语义）：
/// `evt` = 帧完成事件（若有）；`retry` = 本包未被消费——FID 翻转完成帧时
/// 本包属于新帧的第一包，调用者须换目标缓冲后以同一 data 重调（payload
/// 含 SOI 从不因帧边界丢弃）。
#[derive(Debug, Clone, Copy, Default)]
pub struct PushResult {
    pub evt: Option<FrameEvent>,
    pub retry: bool,
}

impl FrameParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 开始一次采集：复位帧进度。
    ///
    /// 若上一次采集超时留下了半帧（`filled > 0`），半帧数据所在的目标缓冲已失效，
    /// 丢弃并重新做 FID 同步（等价于中途加入流）。
    pub fn begin(&mut self) {
        if self.filled != 0 {
            self.filled = 0;
            self.synced = false;
            self.last_fid = None;
        }
    }

    /// 处理一个 microframe 的 UVC 载荷（含头）；payload 直接写入 `dest`。
    ///
    /// 帧边界语义对齐 Linux uvcvideo（uvc_video.c）：FID 翻转完成帧（EOF 丢失
    /// 兜底）+ EOF 完成帧；**不校验内容**（SOI/帧大小完整性交给用户/解码器）；
    /// FID 翻转完成帧时本包不消费（`retry`）——新帧第一包 payload 不得丢弃。
    pub fn push_packet(&mut self, data: &[u8], dest: &mut [u8]) -> PushResult {
        // ① 头 sanity（对齐 uvc_video.c:1197-1200）：非法包丢弃且不影响 FID 状态。
        let (hdr, hdr_len) = match UvcPayloadHeader::parse(data) {
            Some(v) => v,
            None => {
                self.nb_invalid += 1;
                return PushResult::default();
            }
        };

        // ② 帧完成检测：FID 翻转 + 当前帧有数据（对齐 uvc_video.c:1222-1228）。
        //    注释（:1205-1221）：EOF 位比 FID 翻转早一帧，正常帧由 EOF（⑥）
        //    完成，此处处理 EOF 丢失/设备不置 EOF 的兜底。
        //    **本包属于新帧——不消费，返回 retry**（调用者换目标缓冲后重调
        //    同一包，对齐 :1590-1595 的 -EAGAIN + uvc_video_next_buffers）。
        let fid_toggle = self.last_fid.is_some_and(|last| last != hdr.fid);
        if fid_toggle && self.filled > 0 {
            let evt = self.finish_frame(hdr.fid);
            return PushResult {
                evt: Some(evt),
                retry: true,
            };
        }

        // ③ 流同步（对齐 uvc_video.c:1303-1317）：未同步时等待首次 FID 翻转
        //    （避免 STREAMON 后加入抓到半帧）；同步后记录 FID 供下一次边界检测。
        if !self.synced {
            match self.last_fid {
                None => {
                    self.last_fid = Some(hdr.fid);
                    return PushResult::default();
                }
                Some(last) if last == hdr.fid => return PushResult::default(),
                Some(_) => self.synced = true,
            }
        }
        self.last_fid = Some(hdr.fid);

        // ④ ERR 位：只统计不丢帧（
        if hdr.has_err {
            self.error_packet_count += 1;
        }

        // ⑤ payload 写入目标缓冲
        let payload = &data[hdr_len..];
        let room = dest.len() - self.filled;
        let take = payload.len().min(room);
        dest[self.filled..self.filled + take].copy_from_slice(&payload[..take]);
        self.filled += take;

        if let Some(pts) = hdr.pts {
            self.last_pts = Some(pts);
        }

        // ⑥ EOF 完成
        if hdr.eof && self.filled > 0 {
            let evt = self.finish_frame(hdr.fid);
            return PushResult {
                evt: Some(evt),
                retry: false,
            };
        }

        PushResult::default()
    }

    /// 完成当前帧并返回事件
    fn finish_frame(&mut self, fid: bool) -> FrameEvent {
        let evt = FrameEvent {
            bytes: self.filled,
            pts_90khz: self.last_pts.take(),
            eof: true,
            fid,
            frame_number: self.frame_number,
        };
        if evt.bytes > 0 {
            self.frame_number = self.frame_number.wrapping_add(1);
        }
        self.filled = 0;
        evt
    }
}

impl FrameParser {
    /// 携带 ERR 标志的 UVC 包计数（任务侧 trace 用）。
    pub fn error_packet_count(&self) -> u32 {
        self.error_packet_count
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    const FID0: u8 = 0;
    const FID1: u8 = Flags::FID.bits();
    const EOF: u8 = Flags::EOF.bits();
    const ERR: u8 = Flags::ERR.bits();

    /// 构造一个 2 字节头 + payload 的 UVC 载荷。
    fn pkt(flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(2 + payload.len());
        v.push(2);
        v.push(flags);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn sync_discards_until_first_fid_toggle() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 1024];
        // 同步前：同 FID 数据全部丢弃，目标缓冲保持全零（对齐 Linux :1303-1311）
        assert!(p.push_packet(&pkt(FID0, b"aaa"), &mut dest).evt.is_none());
        assert!(p.push_packet(&pkt(FID0, b"bbb"), &mut dest).evt.is_none());
        assert_eq!(dest, vec![0u8; 1024]);
        // 第一次翻转：开始收帧（payload 直接写，不校验 SOI）
        assert!(
            p.push_packet(&pkt(FID1, b"\xFF\xD8c"), &mut dest)
                .evt
                .is_none()
        );
        let r = p.push_packet(&pkt(FID1 | EOF, b"dd\xFF\xD9"), &mut dest);
        let evt = r.evt.unwrap();
        assert_eq!(evt.bytes, 7);
        assert_eq!(&dest[..7], b"\xFF\xD8cdd\xFF\xD9");
        assert!(evt.eof);
        assert!(!r.retry);
    }

    #[test]
    fn frame_completes_on_eof() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 1024];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        // 翻转后无 EOF 不出帧
        assert!(
            p.push_packet(&pkt(FID1, b"\xFF\xD8bb"), &mut dest)
                .evt
                .is_none()
        );
        assert!(p.push_packet(&pkt(FID1, b"cc"), &mut dest).evt.is_none());
        let r = p.push_packet(&pkt(FID1 | EOF, b"dd\xFF\xD9"), &mut dest);
        let evt = r.evt.unwrap();
        assert_eq!(evt.bytes, 10);
        assert_eq!(&dest[..10], b"\xFF\xD8bbccdd\xFF\xD9");
        assert!(!r.retry);
    }

    #[test]
    fn full_buffer_truncates_and_waits_for_eof() {
        // 对齐 Linux decode_data：数据超缓冲截断，无"写满即完成"——等 EOF/FID。
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 8];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        // 写满 dest（截断，多余字节丢弃）
        let r = p.push_packet(&pkt(FID1, b"\xFF\xD8123456789"), &mut dest);
        assert!(r.evt.is_none());
        assert_eq!(dest, b"\xFF\xD8123456");
        // EOF 完成（filled=8 > 0）
        let r = p.push_packet(&pkt(FID1 | EOF, b"xy"), &mut dest);
        let evt = r.evt.unwrap();
        assert_eq!(evt.bytes, 8);
        assert!(!r.retry);
    }

    #[test]
    fn fid_toggle_completes_frame_and_retries_packet() {
        // 核心（对齐 Linux -EAGAIN + uvc_video_next_buffers）：EOF 丢失时
        // FID 翻转完成帧，**本包不消费**（retry）——调用者换目标缓冲后重调，
        // 新帧第一包 payload（含 SOI）保留，不再连锁丢帧。
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 64];
        assert!(!p.push_packet(&pkt(FID0, b"x"), &mut dest).retry);
        // 帧 1：无 EOF（模拟 EOF 包丢失）
        assert!(
            p.push_packet(&pkt(FID1, b"\xFF\xD8f1\xFF\xD9"), &mut dest)
                .evt
                .is_none()
        );
        // FID 翻转：完成帧 1 + retry（本包 = 帧 2 第一包）
        let r = p.push_packet(&pkt(FID0, b"\xFF\xD8f2"), &mut dest);
        let evt = r.evt.unwrap();
        assert_eq!(evt.bytes, 6); // b"\xFF\xD8f1\xFF\xD9" = 6B
        assert_eq!(&dest[..6], b"\xFF\xD8f1\xFF\xD9");
        assert!(r.retry);
        // 调用者换目标缓冲重调同一包
        let mut dest2 = vec![0u8; 64];
        let r2 = p.push_packet(&pkt(FID0, b"\xFF\xD8f2"), &mut dest2);
        assert!(r2.evt.is_none());
        assert!(!r2.retry);
        assert_eq!(&dest2[..4], b"\xFF\xD8f2"); // SOI 保留！
        // EOF 完成帧 2（4B + 6B = 10B）
        let r3 = p.push_packet(&pkt(FID0 | EOF, b"tail\xFF\xD9"), &mut dest2);
        let evt3 = r3.evt.unwrap();
        assert_eq!(evt3.bytes, 10);
        assert_eq!(&dest2[..10], b"\xFF\xD8f2tail\xFF\xD9");
    }

    #[test]
    fn no_soi_frame_still_delivered() {
        // 对齐 Linux：不校验内容——帧不以 FFD8 开头也照常交付（用户/解码器判断）。
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 64];
        assert!(!p.push_packet(&pkt(FID0, b"x"), &mut dest).retry);
        assert!(p.push_packet(&pkt(FID1, b"junk"), &mut dest).evt.is_none());
        let r = p.push_packet(&pkt(FID1 | EOF, b"tail"), &mut dest);
        let evt = r.evt.unwrap();
        assert_eq!(evt.bytes, 8);
        assert_eq!(&dest[..8], b"junktail");
    }

    #[test]
    fn err_packet_counts_without_dropping_frame() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 1024];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        // ERR 包数据照常拼装（对齐 Linux :1287-1292：标记 error 不丢帧）
        assert!(
            p.push_packet(&pkt(FID1 | ERR, b"\xFF\xD8bb"), &mut dest)
                .evt
                .is_none()
        );
        assert!(p.push_packet(&pkt(FID1, b"cc"), &mut dest).evt.is_none());
        let r = p.push_packet(&pkt(FID1 | EOF, b"dd\xFF\xD9"), &mut dest);
        assert_eq!(r.evt.unwrap().bytes, 10);
        assert!(p.error_packet_count > 0);
    }

    #[test]
    fn empty_eof_no_event() {
        // 对齐 Linux :1397（EOF && bytesused != 0）：空 EOF 包不发事件。
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 1024];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        let r = p.push_packet(&pkt(FID1 | EOF, b""), &mut dest);
        assert!(r.evt.is_none());
        assert!(!r.retry);
        // 后续数据正常收帧
        assert!(
            p.push_packet(&pkt(FID1, b"\xFF\xD8bb"), &mut dest)
                .evt
                .is_none()
        );
        let r = p.push_packet(&pkt(FID1 | EOF, b"cc\xFF\xD9"), &mut dest);
        assert_eq!(r.evt.unwrap().bytes, 8);
    }

    #[test]
    fn begin_discards_partial_frame_and_resyncs() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 16];
        assert!(!p.push_packet(&pkt(FID0, b"a"), &mut dest).retry);
        assert!(p.push_packet(&pkt(FID1, b"12345"), &mut dest).evt.is_none());
        assert_eq!(p.filled, 5);
        // 模拟上次 capture 超时后重新采集：丢弃半帧并重新同步
        p.begin();
        assert_eq!(p.filled, 0);
        // 同 FID 数据仍被丢弃（重新等待翻转）
        assert!(p.push_packet(&pkt(FID1, b"xyz"), &mut dest).evt.is_none());
        assert_eq!(p.filled, 0);
        // 翻转后正常收帧
        assert!(p.push_packet(&pkt(FID0, b"abc"), &mut dest).evt.is_none());
        let r = p.push_packet(&pkt(FID0 | EOF, b"def"), &mut dest);
        assert_eq!(r.evt.unwrap().bytes, 6);
        assert_eq!(&dest[..6], b"abcdef");
    }

    #[test]
    fn invalid_header_drops_packet_without_touching_fid_state() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 1024];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        // 声称带 PTS 但 b_length=2 不足 → 非法头，丢弃且不影响 FID 状态
        let bad = vec![2u8, Flags::PTS.bits()];
        assert!(p.push_packet(&bad, &mut dest).evt.is_none());
        let r = p.push_packet(&pkt(FID1 | EOF, b"\xFF\xD8bb\xFF\xD9"), &mut dest);
        assert_eq!(r.evt.unwrap().bytes, 6);
    }

    #[test]
    fn parses_pts_in_extended_header() {
        let v = vec![6u8, Flags::PTS.bits(), 0x78, 0x56, 0x34, 0x12];
        let (hdr, len) = UvcPayloadHeader::parse(&v).unwrap();
        assert_eq!(len, 6);
        assert_eq!(hdr.pts, Some(0x12345678));
    }
}
