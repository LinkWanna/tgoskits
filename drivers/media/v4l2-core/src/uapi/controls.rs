//! V4L2 控制 ID — 相当于 Linux 的 `include/uapi/linux/v4l2-controls.h`。

// ── 控制类 ──────────────────────────────────────────────────────

/// V4L2 控制类 ID（CID 的高 16 位）。
///
/// 来自 `linux/v4l2-controls.h`。控制按功能类组织；
/// 类别编码在控制 ID 的 31:16 位。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlClass {
    User        = 0x00980000,
    Codec       = 0x00990000,
    Camera      = 0x009a0000,
    FmTx        = 0x009b0000,
    Flash       = 0x009c0000,
    Jpeg        = 0x009d0000,
    ImageSource = 0x009e0000,
    ImageProc   = 0x009f0000,
    Dv          = 0x00a00000,
    FmRx        = 0x00a10000,
}

// ── 基偏移 ─────────────────────────────────────────────────────────

/// `V4L2_CID_BASE = (V4L2_CTRL_CLASS_USER | 0x900) = 0x00980900`.
const USER_CID_BASE: u32 = CtrlClass::User as u32 | 0x900;

/// `V4L2_CID_CAMERA_CLASS_BASE = (V4L2_CTRL_CLASS_CAMERA | 0x900) = 0x009a0900`.
const CAMERA_CID_BASE: u32 = CtrlClass::Camera as u32 | 0x900;

// ── 用户类控制 ID ───────────────────────────────────────────────

/// V4L2 用户类控制 ID（`V4L2_CID_BASE` + 偏移）。
///
/// 设计：`V4L2_CID_BRIGHTNESS = (V4L2_CTRL_CLASS_USER | 0x900) + 0`。
///
/// 使用 `as u32` 获取供 `CtrlHandler::find()` 使用的原始 CID。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserClassCtrl {
    Brightness           = USER_CID_BASE,
    Contrast             = USER_CID_BASE + 1,
    Saturation           = USER_CID_BASE + 2,
    Hue                  = USER_CID_BASE + 3,
    AudioVolume          = USER_CID_BASE + 5,
    AudioBalance         = USER_CID_BASE + 6,
    AudioBass            = USER_CID_BASE + 7,
    AudioTreble          = USER_CID_BASE + 8,
    AudioMute            = USER_CID_BASE + 9,
    AudioLoudness        = USER_CID_BASE + 10,
    BlackLevel           = USER_CID_BASE + 11,
    AutoWhiteBalance     = USER_CID_BASE + 12,
    DoWhiteBalance       = USER_CID_BASE + 13,
    RedBalance           = USER_CID_BASE + 14,
    BlueBalance          = USER_CID_BASE + 15,
    Gamma                = USER_CID_BASE + 16,
    Exposure             = USER_CID_BASE + 17,
    Autogain             = USER_CID_BASE + 18,
    Gain                 = USER_CID_BASE + 19,
    Hflip                = USER_CID_BASE + 20,
    Vflip                = USER_CID_BASE + 21,
    PowerLineFrequency   = USER_CID_BASE + 24,
    HueAuto              = USER_CID_BASE + 25,
    WhiteBalanceTemperature = USER_CID_BASE + 26,
    Sharpness            = USER_CID_BASE + 27,
    BacklightCompensation = USER_CID_BASE + 28,
    ChromaAgc            = USER_CID_BASE + 29,
    ColorKiller          = USER_CID_BASE + 30,
    Colorfx              = USER_CID_BASE + 31,
    Autobrightness       = USER_CID_BASE + 32,
    BandStopFilter       = USER_CID_BASE + 33,
    Rotate               = USER_CID_BASE + 34,
    BgColor              = USER_CID_BASE + 35,
    ChromaGain           = USER_CID_BASE + 36,
    Illuminators1        = USER_CID_BASE + 37,
    Illuminators2        = USER_CID_BASE + 38,
    MinBuffersForCapture = USER_CID_BASE + 39,
    MinBuffersForOutput  = USER_CID_BASE + 40,
    AlphaComponent       = USER_CID_BASE + 41,
    ColorfxCbCr          = USER_CID_BASE + 42,
    ColorfxRgb           = USER_CID_BASE + 43,
}

// ── 相机类控制 ID ─────────────────────────────────────────────

/// V4L2 相机类控制 ID（`V4L2_CID_CAMERA_CLASS_BASE` + 偏移）。
///
/// 设计：`V4L2_CID_EXPOSURE_AUTO = (V4L2_CTRL_CLASS_CAMERA | 0x900) + 1`。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraClassCtrl {
    Class                = (CtrlClass::Camera as u32) | 1,
    ExposureAuto         = CAMERA_CID_BASE + 1,
    ExposureAbsolute     = CAMERA_CID_BASE + 2,
    ExposureAutoPriority = CAMERA_CID_BASE + 3,
    PanRelative          = CAMERA_CID_BASE + 4,
    TiltRelative         = CAMERA_CID_BASE + 5,
    PanReset             = CAMERA_CID_BASE + 6,
    TiltReset            = CAMERA_CID_BASE + 7,
    PanAbsolute          = CAMERA_CID_BASE + 8,
    TiltAbsolute         = CAMERA_CID_BASE + 9,
    FocusAbsolute        = CAMERA_CID_BASE + 10,
    FocusRelative        = CAMERA_CID_BASE + 11,
    FocusAuto            = CAMERA_CID_BASE + 12,
    ZoomAbsolute         = CAMERA_CID_BASE + 13,
    ZoomRelative         = CAMERA_CID_BASE + 14,
    ZoomContinuous       = CAMERA_CID_BASE + 15,
    Privacy              = CAMERA_CID_BASE + 16,
    IrisAbsolute         = CAMERA_CID_BASE + 17,
    IrisRelative         = CAMERA_CID_BASE + 18,
    AutoExposureBias     = CAMERA_CID_BASE + 19,
    AutoNPresetWhiteBalance = CAMERA_CID_BASE + 20,
    WideDynamicRange     = CAMERA_CID_BASE + 21,
    ImageStabilization   = CAMERA_CID_BASE + 22,
    IsoSensitivity       = CAMERA_CID_BASE + 23,
    IsoSensitivityAuto   = CAMERA_CID_BASE + 24,
    ExposureMetering     = CAMERA_CID_BASE + 25,
    SceneMode            = CAMERA_CID_BASE + 26,
    ThreeALock           = CAMERA_CID_BASE + 27,
    AutoFocusStart       = CAMERA_CID_BASE + 28,
    AutoFocusStop        = CAMERA_CID_BASE + 29,
    AutoFocusStatus      = CAMERA_CID_BASE + 30,
    AutoFocusRange       = CAMERA_CID_BASE + 31,
    PanSpeed             = CAMERA_CID_BASE + 32,
    TiltSpeed            = CAMERA_CID_BASE + 33,
    CameraOrientation    = CAMERA_CID_BASE + 34,
    CameraSensorRotation = CAMERA_CID_BASE + 35,
    HdrSensorMode        = CAMERA_CID_BASE + 36,
}
