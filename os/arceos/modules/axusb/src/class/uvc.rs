//! USB Video Class (UVC) 驱动。
//!
//! 通过 `Device::control_in/out`（控制传输）+ `Device::open_endpoint_with` +
//! `TransferRequest::submit`（视频流传输）访问 UVC 摄像头。
//!
//! 包含完整的 UVC 描述符解析、PROBE/COMMIT 协商、帧组装逻辑。

use alloc::{vec, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use ax_driver::prelude::*;
use ax_driver_usb::{
    Direction, EndpointAddress, EndpointInfo, EndpointType, SetupPacket, TransferRequest,
};

use crate::Device;

// ============================================================================
// 常量
// ============================================================================

const VS_PROBE_CONTROL: u8 = 0x01;
const VS_COMMIT_CONTROL: u8 = 0x02;

const USB_DT_CONFIGURATION: u8 = 2;
const USB_DT_INTERFACE: u8 = 4;
const USB_DT_ENDPOINT: u8 = 5;
const CS_INTERFACE: u8 = 0x24;

const VS_FORMAT_MJPEG: u8 = 0x06;
const VS_FRAME_MJPEG: u8 = 0x07;
const VS_FORMAT_UNCOMPRESSED: u8 = 0x04;
const VS_FRAME_UNCOMPRESSED: u8 = 0x05;

const USB_CLASS_VIDEO: u8 = 0x0e;
const USB_SUBCLASS_VIDEO_STREAMING: u8 = 0x02;
const USB_SUBCLASS_VIDEO_CONTROL: u8 = 0x01;

const VC_HEADER: u8 = 0x01;
const VC_INPUT_TERMINAL: u8 = 0x02;
const VC_PROCESSING_UNIT: u8 = 0x05;

const ITT_CAMERA: u16 = 0x0201;

const ENDPOINT_ATTR_ISOCH: u8 = 1;
const ENDPOINT_ATTR_BULK: u8 = 2;

const UVC_PROBE_COMMIT_LEN: usize = 34;

// ============================================================================
// 类型
// ============================================================================

/// 视频流传输类型（VS 接口上的端点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UvcXferKind {
    Bulk,
    Isoch,
}

/// 解析得到的 VS 流参数。
#[derive(Clone, Debug)]
pub struct UvcStreamSelection {
    pub vs_interface: u8,
    pub alt_setting: u8,
    pub ep_num: u8,
    /// `wMaxPacketSize` 原始值（含 HS 带宽倍增位）。
    pub mps_raw: u16,
    pub xfer: UvcXferKind,
    pub format_index: u8,
    pub frame_index: u8,
    pub frame_interval: u32,
    pub is_mjpeg: bool,
    pub frame_w: u16,
    pub frame_h: u16,
    /// PROBE/COMMIT 协商后的 `dwMaxPayloadTransferSize`。
    pub negotiated_payload_size: u32,
    /// PROBE/COMMIT 协商后的 `dwMaxVideoFrameSize`。
    pub negotiated_frame_size: u32,
    /// 同一 ep_num 下所有 Isoch alt 候选。
    pub isoch_alts_count: u8,
    pub isoch_alts: [(u8, u16); 8],
}

/// UVC VideoControl 接口解析结果。
#[derive(Clone, Debug, Default)]
pub struct UvcControlEntities {
    pub vc_interface: u8,
    pub camera_terminal_id: Option<u8>,
    pub ct_controls: u32,
    pub processing_unit_id: Option<u8>,
    pub pu_controls: u32,
}

/// 图像调节覆盖。
#[derive(Clone, Copy, Debug, Default)]
pub struct UvcImageTuning {
    pub brightness: Option<u16>,
    pub contrast: Option<u16>,
    pub hue: Option<u16>,
    pub saturation: Option<u16>,
    pub sharpness: Option<u16>,
    pub gamma: Option<u16>,
    pub backlight: Option<u16>,
    pub gain: Option<u16>,
    pub white_balance_temp_k: Option<u16>,
    pub power_line_freq: Option<u8>,
}

// ============================================================================
// 帧状态（跨 capture 持久化）
// ============================================================================

/// 跨 capture 持久化的「上次 EOF 帧的 FID」。0xFF = 还没抓过。
pub static LAST_EOF_FID: AtomicU8 = AtomicU8::new(0xFF);

/// 重置跨 capture 的连续抓帧状态。
#[inline]
pub fn reset_frame_continuity() {
    LAST_EOF_FID.store(0xFF, Ordering::Relaxed);
}

/// 全局开关：打印微帧级 FID/EOF trace。
pub static FRAME_DEBUG: AtomicBool = AtomicBool::new(false);

/// 像素数上限，0 = 不限制。
pub static PREFERRED_MAX_PIXELS: AtomicU32 = AtomicU32::new(0);

/// 设置 [`PREFERRED_MAX_PIXELS`]。
pub fn set_preferred_max_pixels(p: u32) {
    PREFERRED_MAX_PIXELS.store(p, Ordering::Relaxed);
}

// ============================================================================
// UVC SETUP packet 构造
// ============================================================================

fn setup_get_descriptor_config(cfg_index: u8, w_length: u16) -> SetupPacket {
    SetupPacket::new(0x80, 0x06, (2u16 << 8) | cfg_index as u16, 0, w_length)
}

fn uvc_set_cur_vs(interface: u8, selector: u8, w_length: u16) -> SetupPacket {
    SetupPacket::new(
        0x21,
        0x01,
        (selector as u16) << 8,
        interface as u16,
        w_length,
    )
}

fn uvc_get_cur_vs(interface: u8, selector: u8, w_length: u16) -> SetupPacket {
    SetupPacket::new(
        0xA1,
        0x81,
        (selector as u16) << 8,
        interface as u16,
        w_length,
    )
}

fn uvc_get_max_vs(interface: u8, selector: u8, w_length: u16) -> SetupPacket {
    SetupPacket::new(
        0xA1,
        0x83,
        (selector as u16) << 8,
        interface as u16,
        w_length,
    )
}

fn uvc_set_cur_vc(interface: u8, entity_id: u8, selector: u8, w_length: u16) -> SetupPacket {
    SetupPacket::new(
        0x21,
        0x01,
        (selector as u16) << 8,
        (entity_id as u16) << 8 | interface as u16,
        w_length,
    )
}

fn uvc_get_cur_vc(interface: u8, entity_id: u8, selector: u8, w_length: u16) -> SetupPacket {
    SetupPacket::new(
        0xA1,
        0x81,
        (selector as u16) << 8,
        (entity_id as u16) << 8 | interface as u16,
        w_length,
    )
}

fn uvc_get_def_vc(interface: u8, entity_id: u8, selector: u8, w_length: u16) -> SetupPacket {
    SetupPacket::new(
        0xA1,
        0x87,
        (selector as u16) << 8,
        (entity_id as u16) << 8 | interface as u16,
        w_length,
    )
}

// ============================================================================
// 控制传输 helper
// ============================================================================

fn set_cur_u8(device: &mut Device, vc_if: u8, entity: u8, selector: u8, value: u8) -> bool {
    device
        .control_out(uvc_set_cur_vc(vc_if, entity, selector, 1), &[value])
        .is_ok()
}

fn get_cur_u16(device: &mut Device, vc_if: u8, entity: u8, selector: u8) -> Option<u16> {
    let mut buf = [0u8; 2];
    if device
        .control_in(uvc_get_cur_vc(vc_if, entity, selector, 2), &mut buf)
        .is_ok()
    {
        Some(u16::from_le_bytes(buf))
    } else {
        None
    }
}

fn set_cur_u16(device: &mut Device, vc_if: u8, entity: u8, selector: u8, value: u16) -> bool {
    device
        .control_out(
            uvc_set_cur_vc(vc_if, entity, selector, 2),
            &value.to_le_bytes(),
        )
        .is_ok()
}

fn get_def_u16(device: &mut Device, vc_if: u8, entity: u8, selector: u8) -> Option<u16> {
    let mut buf = [0u8; 2];
    if device
        .control_in(uvc_get_def_vc(vc_if, entity, selector, 2), &mut buf)
        .is_ok()
    {
        Some(u16::from_le_bytes(buf))
    } else {
        None
    }
}

// ============================================================================
// 控制传输 — 高层
// ============================================================================

fn uvc_read_config(device: &mut Device, cfg_index: u8) -> DevResult<Vec<u8>> {
    let mut hdr = [0u8; 9];
    device.control_in(setup_get_descriptor_config(cfg_index, 9), &mut hdr)?;
    if hdr[1] != USB_DT_CONFIGURATION {
        return Err(DevError::Io);
    }
    let total = u16::from_le_bytes([hdr[2], hdr[3]]) as usize;
    if total > 4096 {
        return Err(DevError::Io);
    }
    let mut buf = vec![0u8; total];
    device.control_in(
        setup_get_descriptor_config(cfg_index, total as u16),
        &mut buf,
    )?;
    Ok(buf)
}

fn uvc_start_video_stream(device: &mut Device, sel: &mut UvcStreamSelection) -> DevResult<()> {
    reset_frame_continuity();

    let _ = device.claim_interface(sel.vs_interface, 0);

    let probe_init = build_probe_commit_payload(sel);
    dump_probe("PROBE.SET", &probe_init);

    device.control_out(
        uvc_set_cur_vs(
            sel.vs_interface,
            VS_PROBE_CONTROL,
            UVC_PROBE_COMMIT_LEN as u16,
        ),
        &probe_init,
    )?;

    let mut probe_max = [0u8; UVC_PROBE_COMMIT_LEN];
    let _ = device.control_in(
        uvc_get_max_vs(
            sel.vs_interface,
            VS_PROBE_CONTROL,
            UVC_PROBE_COMMIT_LEN as u16,
        ),
        &mut probe_max,
    );
    dump_probe("PROBE.MAX", &probe_max);

    let mut probe = [0u8; UVC_PROBE_COMMIT_LEN];
    device.control_in(
        uvc_get_cur_vs(
            sel.vs_interface,
            VS_PROBE_CONTROL,
            UVC_PROBE_COMMIT_LEN as u16,
        ),
        &mut probe,
    )?;
    dump_probe("PROBE.CUR", &probe);

    sel.negotiated_payload_size = u32::from_le_bytes([probe[22], probe[23], probe[24], probe[25]]);
    sel.negotiated_frame_size = u32::from_le_bytes([probe[18], probe[19], probe[20], probe[21]]);

    reselect_isoch_alt_for_payload(sel);

    let alt_mps = u32::from(sel.mps_raw & 0x7FF) * (u32::from((sel.mps_raw >> 11) & 0x3) + 1);
    if alt_mps > 0 && alt_mps < sel.negotiated_payload_size {
        info!(
            "UVC: clamping COMMIT payload {} -> {} to match alt bandwidth",
            sel.negotiated_payload_size, alt_mps
        );
        sel.negotiated_payload_size = alt_mps;
        probe[22..26].copy_from_slice(&alt_mps.to_le_bytes());
    }

    device.control_out(
        uvc_set_cur_vs(
            sel.vs_interface,
            VS_COMMIT_CONTROL,
            UVC_PROBE_COMMIT_LEN as u16,
        ),
        &probe,
    )?;

    device.claim_interface(sel.vs_interface, sel.alt_setting)?;

    info!(
        "UVC: streaming armed if={} alt={} payload={} frame_size={}",
        sel.vs_interface, sel.alt_setting, sel.negotiated_payload_size, sel.negotiated_frame_size
    );
    Ok(())
}

fn uvc_init_camera_controls(device: &mut Device, ent: &UvcControlEntities, tune: &UvcImageTuning) {
    let vc_if = ent.vc_interface;

    if let Some(pu) = ent.processing_unit_id {
        let bm = ent.pu_controls;

        if (bm & (1 << 0)) != 0 {
            let want = tune
                .brightness
                .unwrap_or_else(|| get_def_u16(device, vc_if, pu, 0x02).unwrap_or(128));
            let cur = get_cur_u16(device, vc_if, pu, 0x02).unwrap_or(want);
            if cur != want {
                info!("UVC: PU.Brightness {} -> {}", cur, want);
                let _ = set_cur_u16(device, vc_if, pu, 0x02, want);
            }
        }

        if (bm & (1 << 12)) != 0 {
            let _ = set_cur_u8(device, vc_if, pu, 0x0B, 1);
            info!("UVC: PU.WB = Auto");
        }

        if (bm & (1 << 10)) != 0 {
            let plf = tune.power_line_freq.unwrap_or(1);
            let _ = set_cur_u8(device, vc_if, pu, 0x05, plf);
        }
    }

    if let Some(ct) = ent.camera_terminal_id {
        if (ent.ct_controls & (1 << 1)) != 0 {
            for &mode in &[0x02u8, 0x08, 0x04] {
                if set_cur_u8(device, vc_if, ct, 0x02, mode) {
                    info!("UVC: CT.AeMode = {:#04x}", mode);
                    break;
                }
            }

            if (ent.ct_controls & (1 << 2)) != 0 {
                let _ = set_cur_u8(device, vc_if, ct, 0x03, 1);
            }
        }

        if (ent.ct_controls & (1 << 17)) != 0 {
            let _ = set_cur_u8(device, vc_if, ct, 0x08, 1);
        }
    }
}

// ============================================================================
// 描述符解析
// ============================================================================

/// HS 等时：mps_raw & 0x7FF。
#[inline]
fn max_packet_11(mps_raw: u16) -> u32 {
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
                    Some(b) => {
                        if mps > (b.2 & 0x7FF) {
                            tak
                        } else {
                            b
                        }
                    }
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

// ============================================================================
// PROBE/COMMIT payload + alt 重选
// ============================================================================

/// 构造 PROBE/COMMIT payload（34 字节）。
pub fn build_probe_commit_payload(sel: &UvcStreamSelection) -> [u8; UVC_PROBE_COMMIT_LEN] {
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
pub fn dump_probe(prefix: &str, p: &[u8]) {
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
pub fn reselect_isoch_alt_for_payload(sel: &mut UvcStreamSelection) {
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

// ============================================================================
// 视频帧捕获
// ============================================================================

fn parse_uvc_packet(pkt: &[u8]) -> (bool, usize, Option<u8>, u8) {
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

enum FrameState {
    WaitFirstSwitch { last_fid: Option<u8> },
    Capturing { frame_fid: u8, saw_data: bool },
}

fn process_uvc_packet(
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

fn capture_one_frame_via_device(
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

    let mut ep = device.open_endpoint_with(ep_addr, ep_info)?;
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

// ============================================================================
// UvcCamera
// ============================================================================

/// UVC 摄像头实例。
pub struct UvcCamera {
    sel: UvcStreamSelection,
    device: Device,
}

impl UvcCamera {
    pub fn probe(mut device: Device) -> DevResult<Self> {
        let cfg_buf = uvc_read_config(&mut device, 1).map_err(|e| {
            error!("UVC: read_configuration_descriptor err={:?}", e);
            DevError::Io
        })?;
        let cfg_total = cfg_buf.len();

        let mut sel = parse_uvc_video_stream(&cfg_buf[..cfg_total], cfg_total).map_err(|e| {
            error!("UVC: parse_uvc_video_stream err={:?}", e);
            DevError::Io
        })?;

        if let Some(entities) = parse_uvc_control_entities(&cfg_buf[..cfg_total], cfg_total) {
            let tune = UvcImageTuning {
                brightness: Some(96),
                ..Default::default()
            };
            uvc_init_camera_controls(&mut device, &entities, &tune);
        }

        uvc_start_video_stream(&mut device, &mut sel).map_err(|e| {
            error!("UVC: uvc_start_video_stream err={:?}", e);
            DevError::Io
        })?;
        info!(
            "UVC: stream ready {}x{} payload={} frame_size={}",
            sel.frame_w, sel.frame_h, sel.negotiated_payload_size, sel.negotiated_frame_size
        );

        let _ = capture_one_frame_via_device(&mut device, &sel);

        Ok(Self { sel, device })
    }

    pub fn capture_frame(&mut self) -> DevResult<Vec<u8>> {
        const MAX_TRIES: u32 = 8;
        const MIN_VALID_BYTES: usize = 4096;

        for attempt in 0..MAX_TRIES {
            let frame = capture_one_frame_via_device(&mut self.device, &self.sel).map_err(|e| {
                error!(
                    "UVC: capture err={:?} (try {}/{})",
                    e,
                    attempt + 1,
                    MAX_TRIES
                );
                DevError::Io
            })?;

            if frame.len() < MIN_VALID_BYTES {
                warn!(
                    "UVC: frame too small (try {}/{}, size={})",
                    attempt + 1,
                    MAX_TRIES,
                    frame.len()
                );
                reset_frame_continuity();
                continue;
            }

            if frame.len() >= 2
                && frame[0] == 0xFF
                && frame[1] == 0xD8
                && frame[frame.len() - 2] == 0xFF
                && frame[frame.len() - 1] == 0xD9
            {
                info!("UVC: captured {} bytes", frame.len());
                return Ok(frame);
            }

            warn!(
                "UVC: invalid JPEG markers (try {}/{}, size={})",
                attempt + 1,
                MAX_TRIES,
                frame.len()
            );
            reset_frame_continuity();
        }

        Err(DevError::Io)
    }

    pub fn frame_size(&self) -> u32 {
        self.sel.negotiated_frame_size
    }

    pub fn payload_size(&self) -> u32 {
        self.sel.negotiated_payload_size
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn device_mut(&mut self) -> &mut Device {
        &mut self.device
    }
}
