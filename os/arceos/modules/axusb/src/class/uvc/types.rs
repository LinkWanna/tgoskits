//! UVC 数据类型。

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
