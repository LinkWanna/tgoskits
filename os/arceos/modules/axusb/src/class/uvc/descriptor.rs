//! UVC 描述符解析。

use core::sync::atomic::Ordering;

use ax_driver::prelude::*;

use super::{constants::*, types::*};

/// HS 等时：mps_raw & 0x7FF。
#[inline]
pub(crate) fn max_packet_11(mps_raw: u16) -> u32 {
    u32::from(mps_raw & 0x7FF)
}

/// 解析 VS 接口：优先 MJPEG 格式；端点优先 Bulk IN，无则 Isoch IN（取带宽最高 alt）。
pub fn parse_uvc_video_stream(cfg: &[u8], cfg_total: usize) -> DevResult<UvcStreamSelection> {
    let len = cfg_total.min(cfg.len());
    if len < 12 {
        return Err(DevError::Io);
    }
    let mut i = usize::from(cfg[0]);
    if i >= len {
        return Err(DevError::Io);
    }

    let mut cur_ifc_class = 0u8;
    let mut cur_ifc_sub = 0u8;
    let mut cur_ifc_num = 0u8;
    let mut cur_alt = 0u8;

    let mut best_bulk: Option<(u8, u8, u16, u8)> = None;
    let mut best_isoch: Option<(u8, u8, u16, u8)> = None;
    let mut isoch_alts: [(u8, u16); 8] = [(0u8, 0u16); 8];
    let mut isoch_alts_count: usize = 0;

    let mut mjpeg_pick: Option<(u8, u8, u16, u16, u32)> = None;
    let mut uncomp_pick: Option<(u8, u8, u16, u16, u32)> = None;
    let mut cur_fmt_ix_for_frame = 0u8;
    let mut cur_fmt_subtype_for_frame = 0u8;

    while i + 2 <= len {
        let bl = cfg[i] as usize;
        if bl < 2 || i + bl > len {
            break;
        }
        let ty = cfg[i + 1];

        if ty == USB_DT_INTERFACE && bl >= 9 {
            cur_ifc_num = cfg[i + 2];
            cur_alt = cfg[i + 3];
            cur_ifc_class = cfg[i + 5];
            cur_ifc_sub = cfg[i + 6];
        } else if ty == CS_INTERFACE
            && cur_ifc_class == USB_CLASS_VIDEO
            && cur_ifc_sub == USB_SUBCLASS_VIDEO_STREAMING
        {
            let st = cfg.get(i + 2).copied().unwrap_or(0);
            if (st == VS_FORMAT_MJPEG || st == VS_FORMAT_UNCOMPRESSED) && bl >= 4 {
                cur_fmt_subtype_for_frame = st;
                cur_fmt_ix_for_frame = cfg[i + 3];
                info!(
                    "UVC: VS-fmt if={} alt={} ix={} subtype={:#04x} ({})",
                    cur_ifc_num,
                    cur_alt,
                    cur_fmt_ix_for_frame,
                    st,
                    if st == VS_FORMAT_MJPEG {
                        "MJPEG"
                    } else {
                        "Uncompressed"
                    }
                );
            }
            if (st == VS_FRAME_MJPEG || st == VS_FRAME_UNCOMPRESSED) && bl >= 26 {
                let frame_ix = cfg[i + 3];
                let w = u16::from_le_bytes([cfg[i + 5], cfg[i + 6]]);
                let h = u16::from_le_bytes([cfg[i + 7], cfg[i + 8]]);
                let dflt_ival =
                    u32::from_le_bytes([cfg[i + 21], cfg[i + 22], cfg[i + 23], cfg[i + 24]]);
                let ival_type = cfg[i + 25];
                let mut min_ival = dflt_ival;
                if ival_type == 0 && bl >= 38 {
                    let dw_min =
                        u32::from_le_bytes([cfg[i + 26], cfg[i + 27], cfg[i + 28], cfg[i + 29]]);
                    if dw_min > 0 {
                        min_ival = dw_min;
                    }
                } else if ival_type > 0 {
                    let n = ival_type as usize;
                    let mut p = i + 26;
                    for _ in 0..n {
                        if p + 4 > i + bl {
                            break;
                        }
                        let v = u32::from_le_bytes([cfg[p], cfg[p + 1], cfg[p + 2], cfg[p + 3]]);
                        if v > 0 && v < min_ival {
                            min_ival = v;
                        }
                        p += 4;
                    }
                }
                let fps_x100 = if dflt_ival > 0 {
                    1_000_000_00u32 / dflt_ival.max(1)
                } else {
                    0
                };
                let fps_min_x100 = if min_ival > 0 {
                    1_000_000_00u32 / min_ival.max(1)
                } else {
                    0
                };
                info!(
                    "UVC: VS-frame fmt_ix={} frame_ix={} {}x{} iv_dflt={} ({}.{:02} fps) \
                     iv_min={} ({}.{:02} fps) ival_type={}",
                    cur_fmt_ix_for_frame,
                    frame_ix,
                    w,
                    h,
                    dflt_ival,
                    fps_x100 / 100,
                    fps_x100 % 100,
                    min_ival,
                    fps_min_x100 / 100,
                    fps_min_x100 % 100,
                    ival_type
                );
                let dflt_ival = if min_ival > 0 { min_ival } else { dflt_ival };
                let pick = (cur_fmt_ix_for_frame, frame_ix, w, h, dflt_ival);
                let is_mjpeg = cur_fmt_subtype_for_frame == VS_FORMAT_MJPEG || st == VS_FRAME_MJPEG;
                fn rank((_, _, pw, ph, _): (u8, u8, u16, u16, u32)) -> i32 {
                    let w = pw as i32;
                    let h = ph as i32;
                    let area = w * h;
                    let pref_max = PREFERRED_MAX_PIXELS.load(Ordering::Relaxed) as i32;
                    if pref_max > 0 {
                        return if area <= pref_max {
                            area.saturating_add(1_000_000)
                        } else {
                            (-(area - pref_max)).saturating_sub(1_000)
                        };
                    }
                    if w == 1280 && h == 720 {
                        return 1_000_000;
                    }
                    if w == 640 && h == 480 {
                        return 900_000;
                    }
                    if w == 800 && h == 600 {
                        return 800_000;
                    }
                    if w == 1024 && h == 768 {
                        return 750_000;
                    }
                    if w == 320 && h == 240 {
                        return 700_000;
                    }
                    if area <= 1280 * 720 {
                        600_000 - (1280 * 720 - area)
                    } else {
                        100_000 - (area - 1280 * 720)
                    }
                }
                if is_mjpeg {
                    let pick_better = match mjpeg_pick {
                        None => true,
                        Some(prev) => rank(pick) > rank(prev),
                    };
                    if pick_better {
                        mjpeg_pick = Some(pick);
                    }
                } else {
                    let pick_better = match uncomp_pick {
                        None => true,
                        Some(prev) => rank(pick) > rank(prev),
                    };
                    if pick_better {
                        uncomp_pick = Some(pick);
                    }
                }
            }
        } else if ty == USB_DT_ENDPOINT
            && cur_ifc_class == USB_CLASS_VIDEO
            && cur_ifc_sub == USB_SUBCLASS_VIDEO_STREAMING
        {
            let ep_addr = cfg[i + 2];
            let attr = cfg[i + 3];
            let mps_raw = u16::from_le_bytes([cfg[i + 4], cfg[i + 5]]);
            let mps = mps_raw & 0x7FF;
            let mult = ((mps_raw >> 11) & 0x3) + 1;
            let xfer = attr & 0x03;
            if (ep_addr & 0x80) == 0 {
                i += bl;
                continue;
            }
            let ep_num = ep_addr & 0x0F;
            let total = u32::from(mps) * u32::from(mult);
            info!(
                "UVC: VS-cand if={} alt={} ep={} kind={} mps={} mult={} total={}/uframe \
                 mps_raw={:#06x}",
                cur_ifc_num,
                cur_alt,
                ep_num,
                if xfer == ENDPOINT_ATTR_BULK {
                    "Bulk"
                } else if xfer == ENDPOINT_ATTR_ISOCH {
                    "Isoch"
                } else {
                    "Other"
                },
                mps,
                mult,
                total,
                mps_raw
            );
            if xfer == ENDPOINT_ATTR_BULK {
                let tak = (cur_alt, ep_num, mps_raw, cur_ifc_num);
                best_bulk = Some(match best_bulk {
                    None => tak,
                    Some(b) if mps > (b.2 & 0x7FF) => tak,
                    Some(b) => b,
                });
            } else if xfer == ENDPOINT_ATTR_ISOCH {
                let tak = (cur_alt, ep_num, mps_raw, cur_ifc_num);
                let new_mult = mult;
                best_isoch = Some(match best_isoch {
                    None => tak,
                    Some(b) => {
                        let old_mps = b.2 & 0x7FF;
                        let old_mult = ((b.2 >> 11) & 0x3) + 1;
                        let new_score = if new_mult == 1 {
                            10_000_000u32 + u32::from(mps)
                        } else {
                            u32::from(mps) * u32::from(new_mult)
                        };
                        let old_score = if old_mult == 1 {
                            10_000_000u32 + u32::from(old_mps)
                        } else {
                            u32::from(old_mps) * u32::from(old_mult)
                        };
                        if new_score > old_score { tak } else { b }
                    }
                });
                if isoch_alts_count < isoch_alts.len() {
                    isoch_alts[isoch_alts_count] = (cur_alt, mps_raw);
                    isoch_alts_count += 1;
                }
            }
        }

        i += bl;
    }

    let (alt, epn, mps_raw, vs_if, kind) = if let Some((a, e, m, v)) = best_bulk {
        (a, e, m, v, UvcXferKind::Bulk)
    } else if let Some((a, e, m, v)) = best_isoch {
        (a, e, m, v, UvcXferKind::Isoch)
    } else {
        return Err(DevError::Unsupported);
    };

    let (fmt_ix, frame_ix, frame_w, frame_h, interval, is_mjpeg) = match mjpeg_pick {
        Some((fi, frix, w, h, iv)) => (fi, frix, w, h, iv, true),
        None => match uncomp_pick {
            Some((fi, frix, w, h, iv)) => (fi, frix, w, h, iv, false),
            None => (1, 1, 0, 0, 333_333, false),
        },
    };

    info!(
        "UVC: SEL if={} alt={} ep={} {:?} mps_raw={:#06x} fmt_ix={} frame_ix={} {}x{} iv={} \
         mjpeg={}",
        vs_if, alt, epn, kind, mps_raw, fmt_ix, frame_ix, frame_w, frame_h, interval, is_mjpeg
    );

    Ok(UvcStreamSelection {
        vs_interface: vs_if,
        alt_setting: alt,
        ep_num: epn,
        mps_raw,
        xfer: kind,
        format_index: fmt_ix,
        frame_index: frame_ix,
        frame_interval: interval,
        is_mjpeg,
        frame_w,
        frame_h,
        negotiated_payload_size: 0,
        negotiated_frame_size: 0,
        isoch_alts_count: isoch_alts_count as u8,
        isoch_alts,
    })
}

/// 解析 VideoControl 接口实体。
pub fn parse_uvc_control_entities(cfg: &[u8], cfg_total: usize) -> Option<UvcControlEntities> {
    let len = cfg_total.min(cfg.len());
    if len < 12 {
        return None;
    }
    let mut i = usize::from(cfg[0]);
    if i >= len {
        return None;
    }

    let mut cur_ifc_class = 0u8;
    let mut cur_ifc_sub = 0u8;
    let mut out = UvcControlEntities::default();
    let mut found_vc = false;

    while i + 2 <= len {
        let bl = cfg[i] as usize;
        if bl < 2 || i + bl > len {
            break;
        }
        let ty = cfg[i + 1];

        if ty == USB_DT_INTERFACE && bl >= 9 {
            let ifc_num = cfg[i + 2];
            cur_ifc_class = cfg[i + 5];
            cur_ifc_sub = cfg[i + 6];
            if cur_ifc_class == USB_CLASS_VIDEO && cur_ifc_sub == USB_SUBCLASS_VIDEO_CONTROL {
                out.vc_interface = ifc_num;
                found_vc = true;
            }
        } else if ty == CS_INTERFACE
            && cur_ifc_class == USB_CLASS_VIDEO
            && cur_ifc_sub == USB_SUBCLASS_VIDEO_CONTROL
            && bl >= 3
        {
            let st = cfg[i + 2];
            match st {
                VC_HEADER => {}
                VC_INPUT_TERMINAL => {
                    if bl >= 15 {
                        let id = cfg[i + 3];
                        let tt = u16::from_le_bytes([cfg[i + 4], cfg[i + 5]]);
                        if tt == ITT_CAMERA {
                            out.camera_terminal_id = Some(id);
                            let csize = cfg[i + 14] as usize;
                            let cmax = csize.min(bl.saturating_sub(15)).min(4);
                            let mut bm = 0u32;
                            for k in 0..cmax {
                                bm |= u32::from(cfg[i + 15 + k]) << (8 * k);
                            }
                            out.ct_controls = bm;
                        }
                    }
                }
                VC_PROCESSING_UNIT => {
                    if bl >= 9 {
                        let id = cfg[i + 3];
                        let csize = cfg[i + 7] as usize;
                        let cmax = csize.min(bl.saturating_sub(8)).min(4);
                        let mut bm = 0u32;
                        for k in 0..cmax {
                            bm |= u32::from(cfg[i + 8 + k]) << (8 * k);
                        }
                        out.processing_unit_id = Some(id);
                        out.pu_controls = bm;
                    }
                }
                _ => {}
            }
        }

        i += bl;
    }

    if found_vc { Some(out) } else { None }
}
