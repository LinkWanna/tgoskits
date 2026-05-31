//! 高层控制传输：读配置描述符、UVC 协商、相机控制初始化。

use alloc::{vec, vec::Vec};

use ax_driver::prelude::*;

use super::{
    constants::*,
    probe::{build_probe_commit_payload, dump_probe, reselect_isoch_alt_for_payload},
    setup::*,
    types::*,
};
use crate::Device;

/// 读完整配置描述符。
pub(crate) fn uvc_read_config(device: &mut Device, cfg_index: u8) -> DevResult<Vec<u8>> {
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

/// PROBE → GET_CUR → COMMIT → SET_INTERFACE，协商流参数并启动。
pub(crate) fn uvc_start_video_stream(
    device: &mut Device,
    sel: &mut UvcStreamSelection,
) -> DevResult<()> {
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

/// 相机控制初始化：自动白平衡 + 自动曝光 + Power-line 50Hz。
pub(crate) fn uvc_init_camera_controls(
    device: &mut Device,
    ent: &UvcControlEntities,
    tune: &UvcImageTuning,
) {
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
