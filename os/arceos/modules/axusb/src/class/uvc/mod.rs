//! USB Video Class (UVC) 驱动。
//!
//! 通过 `Device::control_in/out`（控制传输）+ `Device::open_endpoint_with` +
//! `TransferRequest::submit`（视频流传输）访问 UVC 摄像头。

mod camera;
mod capture;
mod constants;
mod control;
mod descriptor;
mod probe;
mod setup;
pub mod types;

// ── 公共 API ──

pub use camera::UvcCamera;
pub use constants::{
    FRAME_DEBUG, LAST_EOF_FID, PREFERRED_MAX_PIXELS, reset_frame_continuity,
    set_preferred_max_pixels,
};
pub use descriptor::{parse_uvc_control_entities, parse_uvc_video_stream};
pub use types::{UvcControlEntities, UvcImageTuning, UvcStreamSelection, UvcXferKind};
