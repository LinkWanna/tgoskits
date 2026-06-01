//! 视频帧捕获：packet 解析、JPEG 组装、TransferRequest 循环。

use alloc::{vec, vec::Vec};
use core::sync::atomic::Ordering;

use ax_driver::prelude::{
    Direction, EndpointAddress, EndpointInfo, EndpointType, TransferRequest, *,
};

use super::{constants::LAST_EOF_FID, types::*};
use crate::Device;

// ── packet 解析 ──

pub(crate) fn parse_uvc_packet(pkt: &[u8]) -> (bool, usize, Option<u8>, u8) {
    if pkt.len() < 2 {
        return (false, 0, None, 0);
    }
    let hlen = pkt[0] as usize;
    if hlen < 2 || hlen > pkt.len() {
        return (false, 0, None, 0);
    }
    let info = pkt[1];
    let fid = info & 0x01;
    let payload_len = pkt.len() - hlen;
    let eof = (info & 0x02) != 0;
    (eof, payload_len, Some(fid), info)
}

// ── 帧状态 ──

enum FrameState {
    WaitFirstSwitch { last_fid: Option<u8> },
    Capturing { frame_fid: u8, saw_data: bool },
}

// ── packet 处理 ──

pub(crate) fn process_uvc_packet(
    pkt: &[u8],
    state: &mut FrameState,
    jpeg_data: &mut Vec<u8>,
    jpeg_cap: usize,
) -> DevResult<bool> {
    let (eof, payload_len, fid_opt, _info) = parse_uvc_packet(pkt);
    let Some(cur_fid) = fid_opt else {
        return Ok(false);
    };

    match state {
        FrameState::WaitFirstSwitch { last_fid } => {
            match *last_fid {
                None => *last_fid = Some(cur_fid),
                Some(prev) if prev != cur_fid => {
                    *state = FrameState::Capturing {
                        frame_fid: cur_fid,
                        saw_data: false,
                    };
                    return process_uvc_capturing(
                        pkt,
                        payload_len,
                        eof,
                        cur_fid,
                        state,
                        jpeg_data,
                        jpeg_cap,
                    );
                }
                _ => {}
            }
            Ok(false)
        }
        FrameState::Capturing { .. } => {
            process_uvc_capturing(pkt, payload_len, eof, cur_fid, state, jpeg_data, jpeg_cap)
        }
    }
}

fn process_uvc_capturing(
    pkt: &[u8],
    payload_len: usize,
    eof: bool,
    cur_fid: u8,
    state: &mut FrameState,
    jpeg_data: &mut Vec<u8>,
    jpeg_cap: usize,
) -> DevResult<bool> {
    let FrameState::Capturing {
        frame_fid,
        saw_data,
    } = state
    else {
        return Ok(false);
    };

    if cur_fid != *frame_fid {
        let has_eoi = jpeg_data.len() >= 2
            && jpeg_data[jpeg_data.len() - 2] == 0xff
            && jpeg_data[jpeg_data.len() - 1] == 0xd9;

        if *saw_data && has_eoi {
            return Ok(true);
        }

        jpeg_data.clear();
        *frame_fid = cur_fid;
        *saw_data = false;
    }

    if payload_len > 0 {
        let hlen = pkt[0] as usize;
        let payload = &pkt[hlen..];

        if !*saw_data && (payload.len() < 2 || payload[0] != 0xff || payload[1] != 0xd8) {
            return Ok(false);
        }

        if jpeg_data.len() + payload.len() > jpeg_cap {
            return Err(DevError::NoMemory);
        }
        jpeg_data.extend_from_slice(payload);
        *saw_data = true;
    }

    let has_eoi = jpeg_data.len() >= 2
        && jpeg_data[jpeg_data.len() - 2] == 0xff
        && jpeg_data[jpeg_data.len() - 1] == 0xd9;
    let frame_done = eof && *saw_data && has_eoi;

    if eof && *saw_data && !has_eoi {
        jpeg_data.clear();
        *state = FrameState::WaitFirstSwitch {
            last_fid: Some(cur_fid),
        };
        return Ok(false);
    }

    Ok(frame_done)
}

// ── 捕获一帧 ──

pub(crate) fn capture_one_frame_via_device(
    device: &mut Device,
    sel: &UvcStreamSelection,
) -> DevResult<Vec<u8>> {
    let ep_addr = 0x80 | sel.ep_num;
    let mps = (sel.mps_raw & 0x7FF) as usize;
    let mult = ((sel.mps_raw >> 11) & 0x3) as usize + 1;
    let max_work: usize = if mult == 1 { mps } else { mps * mult };
    let jpeg_cap = (sel.negotiated_frame_size as usize).max(320 * 1024);

    let ep_type = match sel.xfer {
        UvcXferKind::Bulk => EndpointType::Bulk,
        UvcXferKind::Isoch => EndpointType::Isochronous,
    };
    let ep_info = EndpointInfo {
        address: EndpointAddress::new(ep_addr),
        transfer_type: ep_type,
        direction: Direction::In,
        max_packet_size: sel.mps_raw,
        packets_per_microframe: mult,
        interval: 0,
    };

    let mut ep = device.open_endpoint(ep_info)?;
    let mut work_buf = vec![0u8; max_work];
    let mut jpeg_data = Vec::with_capacity(jpeg_cap);

    let prev_eof_fid = LAST_EOF_FID.load(Ordering::Relaxed);
    let mut state = FrameState::WaitFirstSwitch {
        last_fid: if prev_eof_fid <= 1 {
            Some(prev_eof_fid)
        } else {
            None
        },
    };

    let max_loops: u32 = match sel.xfer {
        UvcXferKind::Bulk => 60_000,
        UvcXferKind::Isoch => 80_000,
    };

    for _ in 0..max_loops {
        let req = match sel.xfer {
            UvcXferKind::Bulk => {
                let chunk = (mps * 16).min(65536);
                if chunk > work_buf.len() {
                    work_buf.resize(chunk, 0);
                }
                TransferRequest::bulk_in(&mut work_buf[..chunk])
            }
            UvcXferKind::Isoch => TransferRequest::iso_in(&mut work_buf[..max_work], &[max_work]),
        };

        let result = ep.submit(req)?;
        if result.actual_length == 0 {
            continue;
        }

        let pkt = &work_buf[..result.actual_length];

        let frame_done = if mult == 1 || matches!(sel.xfer, UvcXferKind::Bulk) {
            process_uvc_packet(pkt, &mut state, &mut jpeg_data, jpeg_cap)?
        } else {
            let mut hit_eof = false;
            let mut off = 0usize;
            while off < pkt.len() {
                let end = if pkt.len() - off >= mps {
                    off + mps
                } else {
                    pkt.len()
                };
                if process_uvc_packet(&pkt[off..end], &mut state, &mut jpeg_data, jpeg_cap)? {
                    hit_eof = true;
                    break;
                }
                off = end;
            }
            hit_eof
        };

        if frame_done {
            if let FrameState::Capturing { frame_fid, .. } = state {
                LAST_EOF_FID.store(frame_fid, Ordering::Relaxed);
            }
            info!("UVC: captured {} bytes", jpeg_data.len());
            return Ok(jpeg_data);
        }
    }

    Err(DevError::Io)
}
