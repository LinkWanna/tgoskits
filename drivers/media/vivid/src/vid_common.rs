//! vivid-vid-common — 共享格式辅助（镜像 Linux vivid-vid-common.c）。
//!
//! 格式表、尺寸校验与格式协商逻辑，在
//! vid-cap 与（未来的）vid-out 节点之间共享。

use v4l2_core::{
    error::V4l2Error,
    interface::{
        colorspace::{Colorspace, Quantization, XferFunc, YcbcrEncoding},
        Field,
        format::{FmtFlag, Fmtdesc, Format},
    },
};

type Result<T> = core::result::Result<T, V4l2Error>;

// ── 像素格式定义 ─────────────────────────────────────────────

/// vivid 支持的像素格式。
#[derive(Debug, Clone, Copy)]
pub struct VividFormat {
    /// FourCC 编码。
    pub fourcc: u32,
    /// 人类可读的描述（最多 31 字符）。
    pub description: &'static str,
    /// 每像素字节数（半平面格式如 NV12 为 0）。
    pub bpp: u32,
    /// 是否为多平面格式。
    pub is_mplane: bool,
}

/// 支持的单平面像素格式。
pub const VIVID_FORMATS: &[VividFormat] = &[
    VividFormat {
        fourcc: 0x56595559, // YUYV
        description: "YUYV 4:2:2",
        bpp: 2,
        is_mplane: false,
    },
    VividFormat {
        fourcc: 0x33524742, // RGB3 (RGB24)
        description: "RGB3 (RGB24)",
        bpp: 3,
        is_mplane: false,
    },
    VividFormat {
        fourcc: 0x59455247, // GREY
        description: "GREY (8-bit)",
        bpp: 1,
        is_mplane: false,
    },
];

/// 按 FourCC 编码查找格式。
pub fn fmt_by_fourcc(fourcc: u32) -> Option<&'static VividFormat> {
    VIVID_FORMATS.iter().find(|f| f.fourcc == fourcc)
}

/// 按下标查找格式。
pub fn fmt_by_index(index: u32) -> Option<&'static VividFormat> {
    VIVID_FORMATS.get(index as usize)
}

// ── 支持的尺寸 ─────────────────────────────────────────────────

/// 支持的离散帧尺寸（镜像 Linux vivid webcam_sizes）。
pub const SUPPORTED_SIZES: &[(u32, u32)] = &[
    (320, 240),
    (640, 480),
    (800, 600),
    (1280, 720),
    (1920, 1080),
];

/// 每个分辨率下标允许的最大帧间隔（fps）。
/// SUPPORTED_SIZES 的下标 → max_fps（numerator=1）。
const MAX_FPS_BY_SIZE: &[u32] = &[60, 60, 60, 30, 15];

/// 所有可用的离散帧间隔（numerator=1，denominator=fps）。
/// 按从最慢（1/1）到最快（1/120）排列。
pub const SUPPORTED_INTERVALS: &[(u32, u32)] = &[
    (1, 1),   //  1 fps
    (1, 2),   //  2 fps
    (1, 5),   //  5 fps
    (1, 10),  // 10 fps
    (1, 15),  // 15 fps
    (1, 25),  // 25 fps
    (1, 30),  // 30 fps
    (1, 50),  // 50 fps
    (1, 60),  // 60 fps
    (1, 120), // 120 fps
];

/// 获取给定帧尺寸允许的最大 fps。
pub fn max_fps_for_size(width: u32, height: u32) -> u32 {
    let idx = SUPPORTED_SIZES
        .iter()
        .position(|&(w, h)| w == width && h == height);
    match idx {
        Some(i) => MAX_FPS_BY_SIZE[i],
        None => 30, // 未知尺寸：默认 30 fps
    }
}

/// 将帧间隔限制到给定尺寸所允许的范围内。
///
/// 若 numerator 与 denominator 均为 0，则返回 `None`。
pub fn clamp_interval(num: u32, den: u32, width: u32, height: u32) -> Option<(u32, u32)> {
    let max_den = max_fps_for_size(width, height);
    if num == 0 && den == 0 {
        return None; // 无效
    }
    if num == 0 {
        return Some((1, max_den.min(30)));
    }
    let fps = den / num;
    let fps = fps.min(max_den);
    let mut best_den = den;
    let mut best_dist = u32::MAX;
    for &(n, d) in SUPPORTED_INTERVALS {
        if n != num {
            continue;
        }
        let dist = d.abs_diff(fps);
        if dist < best_dist {
            best_dist = dist;
            best_den = d;
        }
    }
    Some((num, best_den))
}

// ── 格式协商辅助 ───────────────────────────────────────────

/// 填充用于格式枚举的 `Fmtdesc`。
pub fn enum_format(fmtdesc: &mut Fmtdesc) -> Result<()> {
    let Some(fmt) = fmt_by_index(fmtdesc.index) else {
        return Err(V4l2Error::InvalidArgument);
    };
    let desc = fmt.description.as_bytes();
    let len = desc.len().min(31);
    fmtdesc.description[..len].copy_from_slice(&desc[..len]);
    fmtdesc.pixelformat = fmt.fourcc;
    fmtdesc.flags = FmtFlag::empty();
    Ok(())
}

/// 校验并约束格式设置（供 try_fmt 和 s_fmt 使用）。
pub fn validate_format(
    width: u32,
    height: u32,
    fourcc: u32,
) -> Result<(u32, u32, u32, &'static VividFormat)> {
    // 约束尺寸
    let w = width.clamp(64, 1920);
    let h = height.clamp(64, 1080);

    // 校验像素格式
    let fmt = fmt_by_fourcc(fourcc).ok_or(V4l2Error::InvalidArgument)?;

    Ok((w, h, fourcc, fmt))
}

/// 为 G_FMT 填充 `Format` 结构体。
pub fn fill_g_fmt(f: &mut Format, w: u32, h: u32, fourcc: u32, bpl: u32, sz: u32) {
    f.fmt.pix.width = w;
    f.fmt.pix.height = h;
    f.fmt.pix.pixelformat = fourcc;
    f.fmt.pix.field = Field::NoField;
    f.fmt.pix.bytesperline = bpl;
    f.fmt.pix.sizeimage = sz;
    f.fmt.pix.colorspace = Colorspace::Srgb;
    f.fmt.pix.ycbcr_enc = YcbcrEncoding::Default as u32;
    f.fmt.pix.quantization = Quantization::Default;
    f.fmt.pix.xfer_func = XferFunc::Default;
}

/// 计算给定格式与尺寸下的 bytesperline 和 sizeimage。
pub fn compute_line_size(fourcc: u32, width: u32, height: u32) -> Option<(u32, u32)> {
    let fmt = fmt_by_fourcc(fourcc)?;
    let bpl = width * fmt.bpp;
    let sz = bpl * height;
    Some((bpl, sz))
}

/// 对 RGB 像素应用 brightness/contrast/saturation/hue 调整。
/// brightness: 0-255（128=中性），contrast: 0-255（128=中性）
/// saturation: 0-255（128=中性），hue: -128..128（0=中性）
pub fn apply_proc_amps(
    r: u8,
    g: u8,
    b: u8,
    bright: u32,
    contrast: u32,
    sat: u32,
    _hue: i32,
) -> (u8, u8, u8) {
    // 转换为 i32 以便处理
    let mut ri = r as i32;
    let mut gi = g as i32;
    let mut bi = b as i32;

    // Brightness：相对中性值 128 的偏移
    if bright != 128 {
        let b_offset = bright as i32 - 128;
        ri = (ri + b_offset).clamp(0, 255);
        gi = (gi + b_offset).clamp(0, 255);
        bi = (bi + b_offset).clamp(0, 255);
    }

    // Contrast：围绕 128（中性值）缩放
    if contrast != 128 {
        let factor = (contrast as i32 * 256) / 128; // 定点数 8.8
        ri = ((((ri - 128) * factor) / 256) + 128).clamp(0, 255);
        gi = ((((gi - 128) * factor) / 256) + 128).clamp(0, 255);
        bi = ((((bi - 128) * factor) / 256) + 128).clamp(0, 255);
    }

    // Saturation：转为灰度并插值（lerp）
    if sat != 128 {
        let factor = (sat as i32 * 256) / 128; // 定点数 8.8
        let gray = (ri * 77 + gi * 150 + bi * 29) / 256;
        ri = (gray + ((ri - gray) * factor) / 256).clamp(0, 255);
        gi = (gray + ((gi - gray) * factor) / 256).clamp(0, 255);
        bi = (gray + ((bi - gray) * factor) / 256).clamp(0, 255);
    }

    (ri as u8, gi as u8, bi as u8)
}

#[cfg(test)]
mod size_tests {
    use v4l2_core::interface::{
        buffer::{Buffer, Exportbuffer, Requestbuffers},
        capability::Capability,
        crop::{Crop, Cropcap, Selection},
        ctrl::{Control, ExtControl, QueryCtrl, Querymenu},
        event::{Event, EventSubscription},
        format::{Fmtdesc, Format, FrameIntervalEnum, FrameSizeEnum},
        inout::{Input, Output},
        stream::StreamParm,
    };

    use super::*;

    // 这些必须与 linux/videodev2.h（RISC-V 64 位）中的 C 结构体大小一致。
    #[test]
    fn test_capability() {
        assert_eq!(core::mem::size_of::<Capability>(), 104);
    }
    #[test]
    fn test_fmtdesc() {
        assert_eq!(core::mem::size_of::<Fmtdesc>(), 64);
    }
    #[test]
    fn test_frame_size_enum() {
        assert_eq!(core::mem::size_of::<FrameSizeEnum>(), 44);
    }
    #[test]
    fn test_frame_interval_enum() {
        assert_eq!(core::mem::size_of::<FrameIntervalEnum>(), 52);
    }
    #[test]
    fn test_format() {
        assert_eq!(core::mem::size_of::<Format>(), 208);
    }
    #[test]
    fn test_requestbuffers() {
        assert_eq!(core::mem::size_of::<Requestbuffers>(), 20);
    }
    #[test]
    fn test_buffer() {
        assert_eq!(core::mem::size_of::<Buffer>(), 88);
    }
    #[test]
    fn test_exportbuffer() {
        assert_eq!(core::mem::size_of::<Exportbuffer>(), 64);
    }
    #[test]
    fn test_cropcap() {
        assert_eq!(core::mem::size_of::<Cropcap>(), 44);
    }
    #[test]
    fn test_crop() {
        assert_eq!(core::mem::size_of::<Crop>(), 20);
    }
    #[test]
    fn test_selection() {
        assert_eq!(core::mem::size_of::<Selection>(), 64);
    }
    #[test]
    fn test_control() {
        assert_eq!(core::mem::size_of::<Control>(), 8);
    }
    #[test]
    fn test_ext_control() {
        assert_eq!(core::mem::size_of::<ExtControl>(), 24);
    }
    #[test]
    fn test_query_ctrl() {
        assert_eq!(core::mem::size_of::<QueryCtrl>(), 68);
    }
    #[test]
    fn test_querymenu() {
        assert_eq!(core::mem::size_of::<Querymenu>(), 44);
    }
    #[test]
    fn test_input() {
        assert_eq!(core::mem::size_of::<Input>(), 80);
    }
    #[test]
    fn test_output() {
        assert_eq!(core::mem::size_of::<Output>(), 72);
    }
    #[test]
    fn test_streamparm() {
        assert_eq!(core::mem::size_of::<StreamParm>(), 204);
    }
    #[test]
    fn test_event_subscription() {
        assert_eq!(core::mem::size_of::<EventSubscription>(), 32);
    }
    #[test]
    fn test_event() {
        assert_eq!(core::mem::size_of::<Event>(), 136);
    }
    #[test]
    fn test_fmtflag_size() {
        assert_eq!(
            core::mem::size_of::<FmtFlag>(),
            4,
            "FmtFlag must be u32-sized"
        );
    }
}
