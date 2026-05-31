//! PROBE/COMMIT payload 构造、打印、Isoch alt 重选。

use super::{constants::*, types::*};

/// 构造 PROBE/COMMIT payload（34 字节）。
pub(crate) fn build_probe_commit_payload(sel: &UvcStreamSelection) -> [u8; UVC_PROBE_COMMIT_LEN] {
    let mut b = [0u8; UVC_PROBE_COMMIT_LEN];
    b[0] = 0x01; // bmHint: lock frame interval
    b[1] = 0x00;
    b[2] = sel.format_index;
    b[3] = sel.frame_index;
    b[4..8].copy_from_slice(&sel.frame_interval.to_le_bytes());
    let w = u32::from(sel.frame_w.max(640));
    let h = u32::from(sel.frame_h.max(480));
    let est = if sel.is_mjpeg {
        w.saturating_mul(h)
    } else {
        w.saturating_mul(h).saturating_mul(2)
    };
    b[18..22].copy_from_slice(&est.to_le_bytes());
    let pkt_total = u32::from(sel.mps_raw & 0x7FF) * (u32::from((sel.mps_raw >> 11) & 0x3) + 1);
    b[22..26].copy_from_slice(&pkt_total.to_le_bytes());
    b
}

/// 打印 PROBE/COMMIT 结果。
pub(crate) fn dump_probe(prefix: &str, p: &[u8]) {
    if p.len() < 26 {
        return;
    }
    let bm_hint = u16::from_le_bytes([p[0], p[1]]);
    let fmt_ix = p[2];
    let frame_ix = p[3];
    let interval = u32::from_le_bytes([p[4], p[5], p[6], p[7]]);
    let key_frm = u16::from_le_bytes([p[8], p[9]]);
    let pframe = u16::from_le_bytes([p[10], p[11]]);
    let comp_q = u16::from_le_bytes([p[12], p[13]]);
    let comp_w = u16::from_le_bytes([p[14], p[15]]);
    let delay = u16::from_le_bytes([p[16], p[17]]);
    let max_video = u32::from_le_bytes([p[18], p[19], p[20], p[21]]);
    let max_pkt = u32::from_le_bytes([p[22], p[23], p[24], p[25]]);
    info!(
        "UVC: {} bmHint={:#06x} fmt={} frame={} iv={} keyFrm={} pFrm={} compQ={} compW={} \
         delay={} dwMaxVideoFrameSize={} dwMaxPayloadTransferSize={}",
        prefix,
        bm_hint,
        fmt_ix,
        frame_ix,
        interval,
        key_frm,
        pframe,
        comp_q,
        comp_w,
        delay,
        max_video,
        max_pkt
    );
}

/// 根据协商 payload 重选最匹配的 Isoch alt（跳过 mult>1，SG2002 兼容）。
pub(crate) fn reselect_isoch_alt_for_payload(sel: &mut UvcStreamSelection) {
    if sel.xfer != UvcXferKind::Isoch || sel.isoch_alts_count == 0 {
        return;
    }
    let need = sel.negotiated_payload_size;
    if need == 0 {
        return;
    }
    let alts = &sel.isoch_alts[..sel.isoch_alts_count as usize];
    let mut best_fit: Option<(u8, u16, u32)> = None;
    let mut best_max: Option<(u8, u16, u32)> = None;
    for &(alt, mps_raw) in alts {
        let mps = u32::from(mps_raw & 0x7FF);
        let mult = u32::from((mps_raw >> 11) & 0x3) + 1;
        if mult > 1 {
            continue;
        }
        let total = mps;
        if total >= need {
            let pick = (alt, mps_raw, total);
            best_fit = Some(match best_fit {
                None => pick,
                Some(p) if p.2 > total => pick,
                Some(p) => p,
            });
        }
        let pick = (alt, mps_raw, total);
        best_max = Some(match best_max {
            None => pick,
            Some(p) if p.2 < total => pick,
            Some(p) => p,
        });
    }
    let (new_alt, new_mps_raw, new_total) =
        best_fit
            .or(best_max)
            .unwrap_or((sel.alt_setting, sel.mps_raw, 0));
    if new_alt != sel.alt_setting || new_mps_raw != sel.mps_raw {
        info!(
            "UVC: re-select Isoch alt {} (mps_raw={:#06x}, {} B/uframe) -> alt {} \
             (mps_raw={:#06x}, {} B/uframe) for payload={} (mult>1 skipped)",
            sel.alt_setting,
            sel.mps_raw,
            u32::from(sel.mps_raw & 0x7FF) * (u32::from((sel.mps_raw >> 11) & 0x3) + 1),
            new_alt,
            new_mps_raw,
            new_total,
            need
        );
        sel.alt_setting = new_alt;
        sel.mps_raw = new_mps_raw;
    }
}
