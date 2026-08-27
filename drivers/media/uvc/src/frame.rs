use bitflags::bitflags;

bitflags! {
    /// 载荷头标志 (2.4.3.3)。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct PayloadHeaderFlags: u8 {
        const EOH = 1 << 7; // End of Header
        const ERR = 1 << 6; // Error
        const STI = 1 << 5; // Still Image
        const RES = 1 << 4; // Reserved
        const SCR = 1 << 3; // Source Clock Reference
        const PTS = 1 << 2; // Presentation Time Stamp
        const EOF = 1 << 1; // End of Frame
        const FID = 1 << 0; // Frame ID
    }
}

/// UVC 载荷头（2.4.3.3）
#[derive(Debug, Clone)]
pub(crate) struct UvcPayloadHeader {
    pub flag: PayloadHeaderFlags,
    #[allow(dead_code)]
    pub pts: Option<u32>,
}

impl Default for UvcPayloadHeader {
    fn default() -> Self {
        Self {
            flag: PayloadHeaderFlags::empty(),
            pts: None,
        }
    }
}

impl UvcPayloadHeader {
    /// 从字节流解析 UVC 载荷头；若数据不合法，返回 None 以允许上层丢弃该包。
    pub(crate) fn parse(buf: &[u8]) -> Option<(Self, usize)> {
        if buf.len() < 2 {
            return None;
        }
        let b_length = buf[0] as usize;
        let flag = PayloadHeaderFlags::from_bits_truncate(buf[1]);
        if b_length < 2 || b_length > buf.len() {
            return None;
        }

        let has_pts = flag.contains(PayloadHeaderFlags::PTS);
        let has_scr = flag.contains(PayloadHeaderFlags::SCR);

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

        if has_scr && offset + 6 > b_length {
            return None;
        }

        let header = UvcPayloadHeader { flag, pts };

        Some((header, b_length))
    }
}

/// UVC 帧解析/组装状态机（零拷贝）。
#[derive(Debug, Default)]
pub(crate) struct FrameParser {
    last_fid: Option<bool>,
    filled: usize,
    synced: bool,
}

/// push_packet 结果（对齐 Linux uvc_video_decode_start 的 `-EAGAIN` 语义）：
/// `bytes` = 帧完成字节数（若有）；`retry` = 本包未被消费——FID 翻转完成帧时
/// 本包属于新帧的第一包，调用者须换目标缓冲后以同一 data 重调（payload
/// 含 SOI 从不因帧边界丢弃）。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PushResult {
    pub bytes: Option<usize>,
    pub retry: bool,
}

impl FrameParser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 处理一个 microframe 的 UVC 载荷（含头）；payload 直接写入 `dest`。
    pub(crate) fn push_packet(&mut self, data: &[u8], dest: &mut [u8]) -> PushResult {
        // ① 头 sanity（对齐 uvc_video.c:1197-1200）：非法包丢弃且不影响 FID 状态。
        let (hdr, hdr_len) = match UvcPayloadHeader::parse(data) {
            Some(v) => v,
            None => return PushResult::default(),
        };

        let fid = hdr.flag.contains(PayloadHeaderFlags::FID);
        let eof = hdr.flag.contains(PayloadHeaderFlags::EOF);
        // ② 帧完成检测：FID 翻转 + 当前帧有数据（对齐 uvc_video.c:1222-1228）。
        let fid_toggle = self.last_fid.is_some_and(|last| last != fid);
        if fid_toggle && self.filled > 0 {
            let bytes = self.finish_frame();
            return PushResult {
                bytes: Some(bytes),
                retry: true,
            };
        }

        // ③ 流同步（对齐 uvc_video.c:1303-1317）：未同步时等待首次 FID 翻转
        if !self.synced {
            match self.last_fid {
                None => {
                    self.last_fid = Some(fid);
                    return PushResult::default();
                }
                Some(last) if last == fid => return PushResult::default(),
                Some(_) => self.synced = true,
            }
        }
        self.last_fid = Some(fid);

        // ⑤ payload 写入目标缓冲
        let payload = &data[hdr_len..];
        let room = dest.len() - self.filled;
        let take = payload.len().min(room);
        dest[self.filled..self.filled + take].copy_from_slice(&payload[..take]);
        self.filled += take;

        // ⑥ EOF 完成
        if eof && self.filled > 0 {
            let bytes = self.finish_frame();
            return PushResult {
                bytes: Some(bytes),
                retry: false,
            };
        }

        PushResult::default()
    }

    fn finish_frame(&mut self) -> usize {
        let bytes = self.filled;
        self.filled = 0;
        bytes
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    const FID0: u8 = 0;
    const FID1: u8 = PayloadHeaderFlags::FID.bits();
    const EOF: u8 = PayloadHeaderFlags::EOF.bits();

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
        assert!(p.push_packet(&pkt(FID0, b"aaa"), &mut dest).bytes.is_none());
        assert!(p.push_packet(&pkt(FID0, b"bbb"), &mut dest).bytes.is_none());
        assert_eq!(dest, vec![0u8; 1024]);
        assert!(
            p.push_packet(&pkt(FID1, b"\xFF\xD8c"), &mut dest)
                .bytes
                .is_none()
        );
        let r = p.push_packet(&pkt(FID1 | EOF, b"dd\xFF\xD9"), &mut dest);
        assert_eq!(r.bytes.unwrap(), 7);
        assert_eq!(&dest[..7], b"\xFF\xD8cdd\xFF\xD9");
        assert!(!r.retry);
    }

    #[test]
    fn frame_completes_on_eof() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 1024];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        assert!(
            p.push_packet(&pkt(FID1, b"\xFF\xD8bb"), &mut dest)
                .bytes
                .is_none()
        );
        assert!(p.push_packet(&pkt(FID1, b"cc"), &mut dest).bytes.is_none());
        let r = p.push_packet(&pkt(FID1 | EOF, b"dd\xFF\xD9"), &mut dest);
        assert_eq!(r.bytes.unwrap(), 10);
        assert_eq!(&dest[..10], b"\xFF\xD8bbccdd\xFF\xD9");
        assert!(!r.retry);
    }

    #[test]
    fn full_buffer_truncates_and_waits_for_eof() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 8];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        let r = p.push_packet(&pkt(FID1, b"\xFF\xD8123456789"), &mut dest);
        assert!(r.bytes.is_none());
        assert_eq!(dest, b"\xFF\xD8123456");
        let r = p.push_packet(&pkt(FID1 | EOF, b"xy"), &mut dest);
        assert_eq!(r.bytes.unwrap(), 8);
        assert!(!r.retry);
    }

    #[test]
    fn fid_toggle_completes_frame_and_retries_packet() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 64];
        assert!(!p.push_packet(&pkt(FID0, b"x"), &mut dest).retry);
        assert!(
            p.push_packet(&pkt(FID1, b"\xFF\xD8f1\xFF\xD9"), &mut dest)
                .bytes
                .is_none()
        );
        let r = p.push_packet(&pkt(FID0, b"\xFF\xD8f2"), &mut dest);
        assert_eq!(r.bytes.unwrap(), 6);
        assert_eq!(&dest[..6], b"\xFF\xD8f1\xFF\xD9");
        assert!(r.retry);
        let mut dest2 = vec![0u8; 64];
        let r2 = p.push_packet(&pkt(FID0, b"\xFF\xD8f2"), &mut dest2);
        assert!(r2.bytes.is_none());
        assert!(!r2.retry);
        assert_eq!(&dest2[..4], b"\xFF\xD8f2");
        let r3 = p.push_packet(&pkt(FID0 | EOF, b"tail\xFF\xD9"), &mut dest2);
        assert_eq!(r3.bytes.unwrap(), 10);
        assert_eq!(&dest2[..10], b"\xFF\xD8f2tail\xFF\xD9");
    }

    #[test]
    fn no_soi_frame_still_delivered() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 64];
        assert!(!p.push_packet(&pkt(FID0, b"x"), &mut dest).retry);
        assert!(
            p.push_packet(&pkt(FID1, b"junk"), &mut dest)
                .bytes
                .is_none()
        );
        let r = p.push_packet(&pkt(FID1 | EOF, b"tail"), &mut dest);
        assert_eq!(r.bytes.unwrap(), 8);
        assert_eq!(&dest[..8], b"junktail");
    }

    #[test]
    fn empty_eof_no_event() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 1024];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        let r = p.push_packet(&pkt(FID1 | EOF, b""), &mut dest);
        assert!(r.bytes.is_none());
        assert!(!r.retry);
        assert!(
            p.push_packet(&pkt(FID1, b"\xFF\xD8bb"), &mut dest)
                .bytes
                .is_none()
        );
        let r = p.push_packet(&pkt(FID1 | EOF, b"cc\xFF\xD9"), &mut dest);
        assert_eq!(r.bytes.unwrap(), 8);
    }

    #[test]
    fn invalid_header_drops_packet_without_touching_fid_state() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 1024];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        let bad = vec![2u8, PayloadHeaderFlags::PTS.bits()];
        assert!(p.push_packet(&bad, &mut dest).bytes.is_none());
        let r = p.push_packet(&pkt(FID1 | EOF, b"\xFF\xD8bb\xFF\xD9"), &mut dest);
        assert_eq!(r.bytes.unwrap(), 6);
    }

    #[test]
    fn parses_pts_in_extended_header() {
        let v = vec![6u8, PayloadHeaderFlags::PTS.bits(), 0x78, 0x56, 0x34, 0x12];
        let (hdr, len) = UvcPayloadHeader::parse(&v).unwrap();
        assert_eq!(len, 6);
        assert_eq!(hdr.pts, Some(0x12345678));
    }
}
