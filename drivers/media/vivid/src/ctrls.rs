//! 控制 ID — 镜像 Linux vivid 的 `vivid-ctrls.c` 自定义 CID 常量。
//!
//! Vivid 定义了自己的控件类（`VIVID_CID_VIVID_BASE = 0x00f0f000`）
//! 位于 `V4L2_CID_PRIVATE_BASE` 之下、典型驱动私有区间之上。

// ── 基础常量 ──────────────────────────────────────────────────────

/// Vivid 自定义控件基址（V4L2_CID_USER_BASE | 0xf000）。
pub const VIVID_CID_CUSTOM_BASE: u32 = 0x0098_f000;

/// Vivid 专用控件类基址。
pub const VIVID_CID_VIVID_BASE: u32 = 0x00f0_f000;
pub const VIVID_CID_VIVID_CLASS: u32 = VIVID_CID_VIVID_BASE | 1;

// ── Vivid 控制 ID ───────────────────────────────────────────────────

/// Vivid 专用控制 ID（`VIVID_CID_VIVID_BASE` + 偏移）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VividCtrl {
    // ── 测试图案 ──
    OsdTextMode         = VIVID_CID_VIVID_BASE + 1,
    HorMovement         = VIVID_CID_VIVID_BASE + 2,
    VertMovement        = VIVID_CID_VIVID_BASE + 3,
    ShowBorder          = VIVID_CID_VIVID_BASE + 4,
    ShowSquare          = VIVID_CID_VIVID_BASE + 5,
    InsertSav           = VIVID_CID_VIVID_BASE + 6,
    InsertEav           = VIVID_CID_VIVID_BASE + 7,
    VbiCapInterlaced    = VIVID_CID_VIVID_BASE + 8,
    InsertHdmiGuardBand = VIVID_CID_VIVID_BASE + 9,

    // ── 处理 ──
    Hflip               = VIVID_CID_VIVID_BASE + 20,
    Vflip               = VIVID_CID_VIVID_BASE + 21,
    StdAspectRatio      = VIVID_CID_VIVID_BASE + 22,
    DvTimingsAspect     = VIVID_CID_VIVID_BASE + 23,
    TstampSrc           = VIVID_CID_VIVID_BASE + 24,

    // ── 色彩空间 ──
    Colorspace          = VIVID_CID_VIVID_BASE + 25,
    XferFunc            = VIVID_CID_VIVID_BASE + 26,
    YcbcrEnc            = VIVID_CID_VIVID_BASE + 27,
    Quantization        = VIVID_CID_VIVID_BASE + 28,
    LimitedRgbRange     = VIVID_CID_VIVID_BASE + 29,
    AlphaMode           = VIVID_CID_VIVID_BASE + 30,

    // ── 能力模拟 ──
    HasCropCap          = VIVID_CID_VIVID_BASE + 31,
    HasComposeCap       = VIVID_CID_VIVID_BASE + 32,
    HasScalerCap        = VIVID_CID_VIVID_BASE + 33,
    HasCropOut          = VIVID_CID_VIVID_BASE + 34,
    HasComposeOut       = VIVID_CID_VIVID_BASE + 35,
    HasScalerOut        = VIVID_CID_VIVID_BASE + 36,

    // ── 杂项 ──
    SeqWrap             = VIVID_CID_VIVID_BASE + 38,
    TimeWrap            = VIVID_CID_VIVID_BASE + 39,
    MaxEdidBlocks       = VIVID_CID_VIVID_BASE + 40,
    PercentageFill      = VIVID_CID_VIVID_BASE + 41,
    ReducedFps          = VIVID_CID_VIVID_BASE + 42,
    HsvEnc              = VIVID_CID_VIVID_BASE + 43,

    // ── 信号模式 ──
    StdSignalMode       = VIVID_CID_VIVID_BASE + 60,
    Standard            = VIVID_CID_VIVID_BASE + 61,
    DvTimingsSignalMode = VIVID_CID_VIVID_BASE + 62,

    // ── 错误注入 ──
    Disconnect          = VIVID_CID_VIVID_BASE + 50,
    DqbufError          = VIVID_CID_VIVID_BASE + 51,
    QueueError          = VIVID_CID_VIVID_BASE + 52,
    PercDropped         = VIVID_CID_VIVID_BASE + 53,

    // ── 用户类空间 ──
    /// Vivid 专用测试图案（非标准的 IMAGE_PROC V4L2_CID_TEST_PATTERN）。
    TestPattern         = 0x00980930,
}

/// `VividCtrl::TestPattern` 菜单项（索引 → 名称）。
pub const TEST_PATTERN_NAMES: &[&str] = &[
    "75% Colorbar",
    "100% Colorbar",
    "CSC Colorbar",
    "Horizontal 100% Colorbar",
    "100% Color Squares",
    "Black",
    "White",
    "Red",
    "Green (Red/Blue Off)",
    "Blue",
    "Alternating Hor. Lines",
    "Alternating Vert. Lines",
    "Cross 1-pixel",
    "Cross 2-pixels",
    "Checkers 16x16",
    "Checkers 2x2",
    "Checkers 1x1",
    "Color Checker 16x16",
];
