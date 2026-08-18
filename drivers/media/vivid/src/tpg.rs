//! V4L2 测试图案生成器 — Linux v4l2-tpg 的实用子集。
//!
//! 无需真实摄像头硬件即可生成测试图案，用于验证 V4L2 采集流水线。
//! 支持常见像素格式以及
//! vivid/vimc 使用的图案。
//!
//! ## 图案
//!
//! | 图案 | 描述 |
//! |---------|------------|
//! | `ColorBars` | SMPTE 75% 彩条（白/黄/青/绿/品红/红/蓝） |
//! | `GradientRamp` | 水平灰度渐变 0→255 |
//! | `Checkerboard` | 32×32 像素黑白交替棋盘 |
//! | `Noise` | 伪随机噪声（按帧播种） |
//! | `Solid(u8, u8, u8)` | 纯色 RGB |
//!
//! ## 像素格式
//!
//! | 格式 | 每像素字节数 | 描述 |
//! |--------|----------------|-------------|
//! | `RGB24` | 3 | 24 位 RGB，打包格式 |
//! | `YUYV` | 2 | YUV 4:2:2，打包格式（YUYV 顺序） |
//! | `NV12` | 1.5 | Y 平面 + 交织 UV 平面（半平面） |

extern crate alloc;

use core::fmt;

/// 预计算正弦表：sin[0] = 127 * sin(-180°)，sin[128] = 127 * sin(0°)。
/// 用于彩条中的色度生成。
/// 复制自 Linux v4l2-tpg-core.c。
#[allow(dead_code)]
const SIN_TABLE: [i8; 257] = [
    0, -4, -7, -11, -13, -18, -20, -22, -26, -29, -33, -35, -37, -41, -43, -48, -50, -52, -56, -58,
    -62, -63, -65, -69, -71, -75, -76, -78, -82, -83, -87, -88, -90, -93, -94, -97, -99, -101,
    -103, -104, -107, -108, -110, -111, -112, -114, -115, -117, -118, -119, -120, -121, -122, -123,
    -123, -124, -125, -125, -126, -126, -127, -127, -127, -127, -127, -127, -127, -127, -126, -126,
    -125, -125, -124, -124, -123, -122, -121, -120, -119, -118, -117, -116, -114, -113, -111, -110,
    -109, -107, -105, -103, -101, -100, -97, -96, -93, -91, -90, -87, -85, -82, -80, -76, -75, -73,
    -69, -67, -63, -62, -60, -56, -54, -50, -48, -46, -41, -39, -35, -33, -31, -26, -24, -20, -18,
    -15, -11, -9, -4, -2, 0, 2, 4, 9, 11, 15, 18, 20, 24, 26, 31, 33, 35, 39, 41, 46, 48, 50, 54,
    56, 60, 62, 64, 67, 69, 73, 75, 76, 80, 82, 85, 87, 90, 91, 93, 96, 97, 100, 101, 103, 105,
    107, 109, 110, 111, 113, 114, 116, 117, 118, 119, 120, 121, 122, 123, 124, 124, 125, 125, 126,
    126, 127, 127, 127, 127, 127, 127, 127, 127, 126, 126, 125, 125, 124, 123, 123, 122, 121, 120,
    119, 118, 117, 115, 114, 112, 111, 110, 108, 107, 104, 103, 101, 99, 97, 94, 93, 90, 88, 87,
    83, 82, 78, 76, 75, 71, 69, 65, 64, 62, 58, 56, 52, 50, 48, 43, 41, 37, 35, 33, 29, 26, 22, 20,
    18, 13, 11, 7, 4, 0,
];

/// 通过正弦表计算 cos(x)。
#[allow(dead_code)]
const fn cos(idx: usize) -> i8 {
    SIN_TABLE[(idx + 64) % 256]
}

/// 每通道 8 位的 RGB 颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// 标准 SMPTE 彩条颜色（75% 幅度）。
pub const COLOR_BAR_WHITE: Rgb8 = Rgb8 {
    r: 191,
    g: 191,
    b: 191,
};
pub const COLOR_BAR_YELLOW: Rgb8 = Rgb8 {
    r: 191,
    g: 191,
    b: 0,
};
pub const COLOR_BAR_CYAN: Rgb8 = Rgb8 {
    r: 0,
    g: 191,
    b: 191,
};
pub const COLOR_BAR_GREEN: Rgb8 = Rgb8 { r: 0, g: 191, b: 0 };
pub const COLOR_BAR_MAGENTA: Rgb8 = Rgb8 {
    r: 191,
    g: 0,
    b: 191,
};
pub const COLOR_BAR_RED: Rgb8 = Rgb8 { r: 191, g: 0, b: 0 };
pub const COLOR_BAR_BLUE: Rgb8 = Rgb8 { r: 0, g: 0, b: 191 };

/// 7 条标准 SMPTE 75% 彩条。
pub const COLOR_BARS: [Rgb8; 7] = [
    COLOR_BAR_WHITE,
    COLOR_BAR_YELLOW,
    COLOR_BAR_CYAN,
    COLOR_BAR_GREEN,
    COLOR_BAR_MAGENTA,
    COLOR_BAR_RED,
    COLOR_BAR_BLUE,
];

/// 100% 幅度 SMPTE 彩条颜色。
pub const COLOR_BAR_100_WHITE: Rgb8 = Rgb8 {
    r: 255,
    g: 255,
    b: 255,
};
pub const COLOR_BAR_100_YELLOW: Rgb8 = Rgb8 {
    r: 255,
    g: 255,
    b: 0,
};
pub const COLOR_BAR_100_CYAN: Rgb8 = Rgb8 {
    r: 0,
    g: 255,
    b: 255,
};
pub const COLOR_BAR_100_GREEN: Rgb8 = Rgb8 { r: 0, g: 255, b: 0 };
pub const COLOR_BAR_100_MAGENTA: Rgb8 = Rgb8 {
    r: 255,
    g: 0,
    b: 255,
};
pub const COLOR_BAR_100_RED: Rgb8 = Rgb8 { r: 255, g: 0, b: 0 };
pub const COLOR_BAR_100_BLUE: Rgb8 = Rgb8 { r: 0, g: 0, b: 255 };

/// 100% 幅度彩条（7 条）。
pub const COLOR_BARS_100: [Rgb8; 7] = [
    COLOR_BAR_100_WHITE,
    COLOR_BAR_100_YELLOW,
    COLOR_BAR_100_CYAN,
    COLOR_BAR_100_GREEN,
    COLOR_BAR_100_MAGENTA,
    COLOR_BAR_100_RED,
    COLOR_BAR_100_BLUE,
];

/// CSC（色彩空间转换）测试图案颜色。
/// 顺序：蓝、红、品红、绿、青、黄、白、黑。
pub const COLOR_BARS_CSC: [Rgb8; 8] = [
    Rgb8 { r: 0, g: 0, b: 255 }, // 蓝
    Rgb8 { r: 255, g: 0, b: 0 }, // 红
    Rgb8 {
        r: 255,
        g: 0,
        b: 255,
    }, // 品红
    Rgb8 { r: 0, g: 255, b: 0 }, // 绿
    Rgb8 {
        r: 0,
        g: 255,
        b: 255,
    }, // 青
    Rgb8 {
        r: 255,
        g: 255,
        b: 0,
    }, // 黄
    Rgb8 {
        r: 255,
        g: 255,
        b: 255,
    }, // 白
    Rgb8 { r: 0, g: 0, b: 0 },   // 黑
];

// ── 像素格式 ─────────────────────────────────────────────────────

/// 支持的输出像素格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 24 位 RGB，按 R-G-B 打包。
    Rgb24,
    /// YUV 4:2:2，按 Y-U-Y-V 打包（每 4 字节两个像素）。
    Yuyv,
    /// Y 平面后跟交织的 U-V 平面（NV12，半平面 4:2:0）。
    Nv12,
}

impl PixelFormat {
    /// 每个“像素单元”的字节数。YUYV 为每 4 字节 2 像素。
    /// 返回 (bytes_per_unit, pixels_per_unit)。
    #[allow(dead_code)]
    const fn unit_size(self) -> (usize, usize) {
        match self {
            Self::Rgb24 => (3, 1),
            Self::Yuyv => (4, 2),
            Self::Nv12 => (1, 1), // 每个 Y 采样
        }
    }

    /// 给定尺寸下的缓冲总大小。
    pub const fn buffer_size(self, width: u32, height: u32) -> usize {
        match self {
            Self::Rgb24 => (width * height * 3) as usize,
            Self::Yuyv => (width * height * 2) as usize,
            Self::Nv12 => (width * height + (width / 2) * (height / 2) * 2) as usize,
        }
    }
}

// ── 图案描述 ───────────────────────────────────────────────

/// 要生成哪种测试图案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// SMPTE 75% 彩条（横向 7 条）。
    ColorBars,
    /// 从 0 到 255 的水平灰度渐变。
    GradientRamp,
    /// 32×32 像素棋盘格（黑白交替）。
    Checkerboard,
    /// 可配置大小的棋盘格（每格 n×n 像素）。
    CheckerboardSized(u32),
    /// 伪随机噪声。
    Noise,
    /// 纯色 RGB。
    Solid(u8, u8, u8),
    /// SMPTE 100% 彩条（满幅度）。
    ColorBars100,
    /// CSC 彩条（蓝/红/品红/绿/青/黄/白/黑）。
    ColorBarsCsc,
    /// 水平 100% 彩条（条带水平排布）。
    ColorBarsHor100,
    /// 白底上的 6×6 彩色方块网格。
    ColorSquares,
    /// 黑白交替水平线（每条 1px）。
    HorLines,
    /// 黑白交替垂直线（每条 1px）。
    VertLines,
    /// 居中十字（水平 + 垂直线），宽度以像素计。
    Cross(u32),
    /// 可配置方块大小（n×n 像素）的彩色棋盘格。
    ColorCheckerboard(u32),
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ColorBars => write!(f, "75% Colorbar"),
            Self::GradientRamp => write!(f, "Gray Ramp"),
            Self::Checkerboard | Self::CheckerboardSized(32) => write!(f, "32x32 Checkers"),
            Self::CheckerboardSized(n) => write!(f, "{0}x{0} Checkers", n),
            Self::Noise => write!(f, "Noise"),
            Self::Solid(r, g, b) => {
                let name = match (*r, *g, *b) {
                    (0, 0, 0) => "Black",
                    (255, 255, 255) => "White",
                    (255, 0, 0) => "Red",
                    (0, 255, 0) => "Green",
                    (0, 0, 255) => "Blue",
                    _ => "Custom",
                };
                write!(f, "{}% {}", if *r > 191 { "100" } else { "75" }, name)
            }
            Self::ColorBars100 => write!(f, "100% Colorbar"),
            Self::ColorBarsCsc => write!(f, "CSC Colorbar"),
            Self::ColorBarsHor100 => write!(f, "Horiz. 100% Colorbar"),
            Self::ColorSquares => write!(f, "100% Color Squares"),
            Self::HorLines => write!(f, "Alternating Hor. Lines"),
            Self::VertLines => write!(f, "Alternating Vert. Lines"),
            Self::Cross(1) => write!(f, "Cross 1-pixel"),
            Self::Cross(n) => write!(f, "Cross {0}-pixels", n),
            Self::ColorCheckerboard(n) => write!(f, "Color Checker {0}x{0}", n),
        }
    }
}

/// 所有可用的图案名称（用于 ENUM 控件）。
pub const PATTERN_NAMES: &[&str] = &[
    "75% Colorbar",
    "Gray Ramp",
    "32x32 Checkers",
    "Noise",
    "Black",
    "White",
    "Red",
    "Green",
    "Blue",
    "100% Colorbar",
    "CSC Colorbar",
    "Horiz. 100% Colorbar",
    "100% Color Squares",
    "Alternating Hor. Lines",
    "Alternating Vert. Lines",
    "Cross 1-pixel",
    "Cross 2-pixels",
    "Checkers 16x16",
    "Checkers 2x2",
    "Checkers 1x1",
    "Color Checker 16x16",
];

/// 按下标获取 Pattern（用于 V4L2 菜单控件）。
pub fn pattern_from_index(index: u32) -> Option<Pattern> {
    match index {
        0 => Some(Pattern::ColorBars),
        1 => Some(Pattern::GradientRamp),
        2 => Some(Pattern::Checkerboard),
        3 => Some(Pattern::Noise),
        4 => Some(Pattern::Solid(0, 0, 0)),
        5 => Some(Pattern::Solid(255, 255, 255)),
        6 => Some(Pattern::Solid(255, 0, 0)),
        7 => Some(Pattern::Solid(0, 255, 0)),
        8 => Some(Pattern::Solid(0, 0, 255)),
        9 => Some(Pattern::ColorBars100),
        10 => Some(Pattern::ColorBarsCsc),
        11 => Some(Pattern::ColorBarsHor100),
        12 => Some(Pattern::ColorSquares),
        13 => Some(Pattern::HorLines),
        14 => Some(Pattern::VertLines),
        15 => Some(Pattern::Cross(1)),
        16 => Some(Pattern::Cross(2)),
        17 => Some(Pattern::CheckerboardSized(16)),
        18 => Some(Pattern::CheckerboardSized(2)),
        19 => Some(Pattern::CheckerboardSized(1)),
        20 => Some(Pattern::ColorCheckerboard(16)),
        _ => None,
    }
}

// ── RGB → YUV 转换 ──────────────────────────────────────────────

/// 用于 YUYV 输出的 RGB 到 YUV 转换。
/// 使用 ITU-R BT.601 系数。
fn rgb_to_yuv(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let r = r as i32;
    let g = g as i32;
    let b = b as i32;
    let y = ((66 * r + 129 * g + 25 * b + 128) >> 8) as u8 + 16;
    let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
    let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
    (y, u.clamp(16, 240) as u8, v.clamp(16, 240) as u8)
}

// ── 噪声帧计数器 ────────────────────────────────────────────

/// 简单的 LCG 伪随机数生成器。
struct Lcg {
    state: u32,
}

impl Lcg {
    const fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u8 {
        self.state = self.state.wrapping_mul(1103515245).wrapping_add(12345);
        (self.state >> 16) as u8
    }
}

// ── 图案生成器 ─────────────────────────────────────────────────

/// 根据图案与帧号获取像素 (x, y) 的颜色。
pub fn pattern_color(pat: Pattern, x: u32, y: u32, w: u32, h: u32, frame: u32) -> Rgb8 {
    match pat {
        Pattern::ColorBars => {
            let bar_w = w / 7;
            let idx = ((x / bar_w) as usize).min(6);
            COLOR_BARS[idx]
        }
        Pattern::GradientRamp => {
            let gray = ((x * 256) / w) as u8;
            Rgb8 {
                r: gray,
                g: gray,
                b: gray,
            }
        }
        Pattern::Checkerboard | Pattern::CheckerboardSized(32) => {
            let cx = (x / 32) % 2;
            let cy = (y / 32) % 2;
            if (cx ^ cy) == 0 {
                Rgb8 {
                    r: 255,
                    g: 255,
                    b: 255,
                }
            } else {
                Rgb8 { r: 0, g: 0, b: 0 }
            }
        }
        Pattern::CheckerboardSized(sz) => {
            let cx = (x / sz) % 2;
            let cy = (y / sz) % 2;
            if (cx ^ cy) == 0 {
                Rgb8 {
                    r: 255,
                    g: 255,
                    b: 255,
                }
            } else {
                Rgb8 { r: 0, g: 0, b: 0 }
            }
        }
        Pattern::Noise => {
            let mut lcg = Lcg::new(
                frame
                    .wrapping_mul(31337)
                    .wrapping_add(y)
                    .wrapping_mul(7)
                    .wrapping_add(x),
            );
            let v = lcg.next();
            Rgb8 { r: v, g: v, b: v }
        }
        Pattern::Solid(r, g, b) => Rgb8 { r, g, b },
        Pattern::ColorBars100 => {
            let bar_w = w / 7;
            let idx = ((x / bar_w) as usize).min(6);
            COLOR_BARS_100[idx]
        }
        Pattern::ColorBarsCsc => {
            let bar_w = w / 8;
            let idx = ((x / bar_w) as usize).min(7);
            COLOR_BARS_CSC[idx]
        }
        Pattern::ColorBarsHor100 => {
            let bar_h = h / 7;
            let idx = ((y / bar_h) as usize).min(6);
            COLOR_BARS_100[idx]
        }
        Pattern::ColorSquares => {
            let cols = 6u32;
            let rows = 6u32;
            let sq_w = w / cols;
            let sq_h = h / rows;
            let cx = x / sq_w;
            let cy = y / sq_h;
            // 白底上的彩色方块
            let colors: &[Rgb8] = &[
                Rgb8 { r: 0, g: 0, b: 0 },   // 黑
                Rgb8 { r: 0, g: 0, b: 255 }, // 蓝
                Rgb8 { r: 255, g: 0, b: 0 }, // 红
                Rgb8 {
                    r: 255,
                    g: 0,
                    b: 255,
                }, // 品红
                Rgb8 { r: 0, g: 255, b: 0 }, // 绿
                Rgb8 {
                    r: 0,
                    g: 255,
                    b: 255,
                }, // 青
                Rgb8 {
                    r: 255,
                    g: 255,
                    b: 0,
                }, // 黄
            ];
            let idx = ((cx + cy * cols) % colors.len() as u32) as usize;
            // 边框格子为白色
            if cx == 0 || cy == 0 || cx == cols - 1 || cy == rows - 1 {
                Rgb8 {
                    r: 255,
                    g: 255,
                    b: 255,
                }
            } else {
                colors[idx]
            }
        }
        Pattern::HorLines => {
            if y.is_multiple_of(2) {
                Rgb8 {
                    r: 255,
                    g: 255,
                    b: 255,
                }
            } else {
                Rgb8 { r: 0, g: 0, b: 0 }
            }
        }
        Pattern::VertLines => {
            if x.is_multiple_of(2) {
                Rgb8 {
                    r: 255,
                    g: 255,
                    b: 255,
                }
            } else {
                Rgb8 { r: 0, g: 0, b: 0 }
            }
        }
        Pattern::Cross(thick) => {
            let cx = w / 2;
            let cy = h / 2;
            let half = thick / 2;
            let dx = if x >= cx - half && x <= cx + half {
                0i32
            } else {
                1
            };
            let dy = if y >= cy - half && y <= cy + half {
                0i32
            } else {
                1
            };
            if dx == 0 || dy == 0 {
                Rgb8 {
                    r: 255,
                    g: 255,
                    b: 255,
                }
            } else {
                Rgb8 { r: 0, g: 0, b: 0 }
            }
        }
        Pattern::ColorCheckerboard(sz) => {
            let colors: &[Rgb8] = &[
                Rgb8 { r: 255, g: 0, b: 0 }, // 红
                Rgb8 { r: 0, g: 255, b: 0 }, // 绿
                Rgb8 { r: 0, g: 0, b: 255 }, // 蓝
                Rgb8 {
                    r: 0,
                    g: 255,
                    b: 255,
                }, // 青
                Rgb8 {
                    r: 255,
                    g: 0,
                    b: 255,
                }, // 品红
                Rgb8 {
                    r: 255,
                    g: 255,
                    b: 0,
                }, // 黄
            ];
            let cx = x / sz;
            let cy = y / sz;
            let idx = ((cx + cy) % colors.len() as u32) as usize;
            colors[idx]
        }
    }
}

// ── 缓冲填充 ────────────────────────────────────────────────────────

/// 用测试图案数据填充缓冲。
///
/// # 参数
/// * `pattern` —— 要生成的图案
/// * `format` —— 输出像素格式
/// * `width`、`height` —— 帧尺寸
/// * `frame` —— 帧序号（用于噪声播种、动态效果）
/// * `buf` —— 输出缓冲（至少 `format.buffer_size(width, height)` 字节）
pub fn fill_buffer(
    pattern: Pattern,
    format: PixelFormat,
    width: u32,
    height: u32,
    frame: u32,
    buf: &mut [u8],
) {
    match format {
        PixelFormat::Rgb24 => fill_rgb24(pattern, width, height, frame, buf),
        PixelFormat::Yuyv => fill_yuyv(pattern, width, height, frame, buf),
        PixelFormat::Nv12 => fill_nv12(pattern, width, height, frame, buf),
    }
}

fn fill_rgb24(pat: Pattern, w: u32, h: u32, frame: u32, buf: &mut [u8]) {
    for y in 0..h {
        for x in 0..w {
            let c = pattern_color(pat, x, y, w, h, frame);
            let idx = ((y * w + x) * 3) as usize;
            buf[idx] = c.r;
            buf[idx + 1] = c.g;
            buf[idx + 2] = c.b;
        }
    }
}

fn fill_yuyv(pat: Pattern, w: u32, h: u32, frame: u32, buf: &mut [u8]) {
    for y in 0..h {
        for x in (0..w).step_by(2) {
            let c0 = pattern_color(pat, x, y, w, h, frame);
            let c1 = pattern_color(pat, x + 1, y, w, h, frame);
            let (y0, u0, v0) = rgb_to_yuv(c0.r, c0.g, c0.b);
            let (y1, u1, v1) = rgb_to_yuv(c1.r, c1.g, c1.b);
            let u = ((u0 as u16 + u1 as u16) / 2) as u8;
            let v = ((v0 as u16 + v1 as u16) / 2) as u8;
            let idx = ((y * w + x) * 2) as usize;
            buf[idx] = y0;
            buf[idx + 1] = u;
            buf[idx + 2] = y1;
            buf[idx + 3] = v;
        }
    }
}

fn fill_nv12(pat: Pattern, w: u32, h: u32, frame: u32, buf: &mut [u8]) {
    let y_size = (w * h) as usize;
    let _uv_size = ((w / 2) * (h / 2)) as usize;

    // Y 平面
    for y in 0..h {
        for x in 0..w {
            let c = pattern_color(pat, x, y, w, h, frame);
            let (lum, ..) = rgb_to_yuv(c.r, c.g, c.b);
            buf[(y * w + x) as usize] = lum;
        }
    }

    // UV 平面（水平与垂直方向各 2 倍下采样）
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let c00 = pattern_color(pat, x, y, w, h, frame);
            let c01 = pattern_color(pat, x + 1, y, w, h, frame);
            let c10 = pattern_color(pat, x, y + 1, w, h, frame);
            let c11 = pattern_color(pat, x + 1, y + 1, w, h, frame);
            let (_, u00, v00) = rgb_to_yuv(c00.r, c00.g, c00.b);
            let (_, u01, v01) = rgb_to_yuv(c01.r, c01.g, c01.b);
            let (_, u10, v10) = rgb_to_yuv(c10.r, c10.g, c10.b);
            let (_, u11, v11) = rgb_to_yuv(c11.r, c11.g, c11.b);
            let u = ((u00 as u16 + u01 as u16 + u10 as u16 + u11 as u16) / 4) as u8;
            let v = ((v00 as u16 + v01 as u16 + v10 as u16 + v11 as u16) / 4) as u8;
            let uv_idx = y_size + ((y / 2) * (w / 2) + (x / 2)) as usize * 2;
            buf[uv_idx] = u;
            buf[uv_idx + 1] = v;
        }
    }
}

// ── 测试 ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_size_rgb24() {
        let size = PixelFormat::Rgb24.buffer_size(640, 480);
        assert_eq!(size, 640 * 480 * 3);
    }

    #[test]
    fn test_buffer_size_yuyv() {
        let size = PixelFormat::Yuyv.buffer_size(640, 480);
        assert_eq!(size, 640 * 480 * 2);
    }

    #[test]
    fn test_buffer_size_nv12() {
        let size = PixelFormat::Nv12.buffer_size(640, 480);
        assert_eq!(size, 640 * 480 + (320 * 240 * 2));
    }

    #[test]
    fn test_color_bars_rgb24() {
        let w = 700u32;
        let h = 4u32;
        let size = PixelFormat::Rgb24.buffer_size(w, h);
        let mut buf = alloc::vec![0u8; size];
        fill_buffer(Pattern::ColorBars, PixelFormat::Rgb24, w, h, 0, &mut buf);

        // 第一条（白色）：x=50 处的像素应为白色
        let idx = 50 * 3;
        assert_eq!(buf[idx], 191);
        assert_eq!(buf[idx + 1], 191);
        assert_eq!(buf[idx + 2], 191);

        // 最后一条（蓝色）：x=650 处的像素应为蓝色
        let idx = 650 * 3;
        assert_eq!(buf[idx], 0);
        assert_eq!(buf[idx + 1], 0);
        assert_eq!(buf[idx + 2], 191);
    }

    #[test]
    fn test_color_bars_yuyv() {
        let w = 700u32;
        let h = 4u32;
        let size = PixelFormat::Yuyv.buffer_size(w, h);
        let mut buf = alloc::vec![0u8; size];
        fill_buffer(Pattern::ColorBars, PixelFormat::Yuyv, w, h, 0, &mut buf);

        // YUYV 应产生非零值
        assert!(buf.iter().any(|&b| b > 0));
    }

    #[test]
    fn test_gradient_ramp() {
        let w = 256u32;
        let h = 1u32;
        let size = PixelFormat::Rgb24.buffer_size(w, h);
        let mut buf = alloc::vec![0u8; size];
        fill_buffer(Pattern::GradientRamp, PixelFormat::Rgb24, w, h, 0, &mut buf);

        // 第一个像素应接近黑色
        assert!(buf[0] < 5);
        // 最后一个像素应接近白色
        assert!(buf[((w - 1) * 3) as usize] > 250);
    }

    #[test]
    fn test_checkerboard() {
        let w = 64u32;
        let h = 64u32;
        let size = PixelFormat::Rgb24.buffer_size(w, h);
        let mut buf = alloc::vec![0u8; size];
        fill_buffer(Pattern::Checkerboard, PixelFormat::Rgb24, w, h, 0, &mut buf);

        // 左上角应为白色（第一个 32x32 块中的 (0,0)）
        assert_eq!(buf[0], 255);
        // (33, 0) 处的像素应为黑色（水平方向下一个 32x32 块中）
        assert_eq!(buf[(33 * 3) as usize], 0);
    }

    #[test]
    fn test_noise_varied() {
        let w = 64u32;
        let h = 64u32;
        let size = PixelFormat::Rgb24.buffer_size(w, h);
        let mut buf1 = alloc::vec![0u8; size];
        let mut buf2 = alloc::vec![0u8; size];
        fill_buffer(Pattern::Noise, PixelFormat::Rgb24, w, h, 0, &mut buf1);
        fill_buffer(Pattern::Noise, PixelFormat::Rgb24, w, h, 1, &mut buf2);

        // 不同帧应产生不同的噪声
        assert_ne!(buf1, buf2);
    }

    #[test]
    fn test_solid() {
        let w = 32u32;
        let h = 32u32;
        let size = PixelFormat::Rgb24.buffer_size(w, h);
        let mut buf = alloc::vec![0u8; size];
        fill_buffer(
            Pattern::Solid(128, 64, 32),
            PixelFormat::Rgb24,
            w,
            h,
            0,
            &mut buf,
        );

        // 每个像素都应相同
        for i in (0..size).step_by(3) {
            assert_eq!(buf[i], 128);
            assert_eq!(buf[i + 1], 64);
            assert_eq!(buf[i + 2], 32);
        }
    }

    #[test]
    fn test_pattern_from_index() {
        assert!(matches!(pattern_from_index(0), Some(Pattern::ColorBars)));
        assert!(matches!(pattern_from_index(3), Some(Pattern::Noise)));
        assert!(matches!(
            pattern_from_index(8),
            Some(Pattern::Solid(0, 0, 255))
        ));
        assert!(matches!(pattern_from_index(99), None));
    }
}
