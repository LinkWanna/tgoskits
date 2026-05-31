//! UVC 摄像头实例。

use alloc::vec::Vec;

use ax_driver::prelude::*;

use super::{
    capture::capture_one_frame_via_device,
    constants::*,
    control::{uvc_init_camera_controls, uvc_read_config, uvc_start_video_stream},
    descriptor::{parse_uvc_control_entities, parse_uvc_video_stream},
    types::*,
};
use crate::Device;

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
