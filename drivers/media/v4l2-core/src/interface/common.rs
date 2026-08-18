//! 共享的 V4L2 ioctl 接口类型。

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// 分数（分子/分母）。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Fract {
    pub numerator: u32,
    pub denominator: u32,
}

/// 场顺序。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    /// 驱动可在无、顶场、底场、隔行中自行选择。
    Any          = 0,
    /// 该设备没有场。
    NoField      = 1,
    /// 仅顶场。
    Top          = 2,
    /// 仅底场。
    Bottom       = 3,
    /// 两场隔行（interlaced）。
    Interlaced   = 4,
    /// 两场顺序，先顶后底。
    SeqTb        = 5,
    /// 两场顺序，先底后顶。
    SeqBt        = 6,
    /// 两场交替放入独立的缓冲区。
    Alternate    = 7,
    /// 两场隔行，顶场在前，先传输顶场。
    InterlacedTb = 8,
    /// 两场隔行，顶场在前，先传输底场。
    InterlacedBt = 9,
}

impl Field {
    /// 若该 Field 包含顶场则返回 true。
    pub const fn has_top(self) -> bool {
        matches!(
            self,
            Self::Top
                | Self::Interlaced
                | Self::InterlacedTb
                | Self::InterlacedBt
                | Self::SeqTb
                | Self::SeqBt
        )
    }

    /// 若该 Field 包含底场则返回 true。
    pub const fn has_bottom(self) -> bool {
        matches!(
            self,
            Self::Bottom
                | Self::Interlaced
                | Self::InterlacedTb
                | Self::InterlacedBt
                | Self::SeqTb
                | Self::SeqBt
        )
    }

    /// 若该 Field 是隔行则返回 true。
    pub const fn is_interlaced(self) -> bool {
        matches!(
            self,
            Self::Interlaced | Self::InterlacedTb | Self::InterlacedBt
        )
    }

    /// 若该 Field 是顺序则返回 true。
    pub const fn is_sequential(self) -> bool {
        matches!(self, Self::SeqTb | Self::SeqBt)
    }
}

/// 缓冲区 / 流类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufType {
    VideoCapture       = 1,
    VideoOutput        = 2,
    VideoOverlay       = 3,
    VbiCapture         = 4,
    VbiOutput          = 5,
    SlicedVbiCapture   = 6,
    SlicedVbiOutput    = 7,
    VideoOutputOverlay = 8,
    VideoCaptureMplane = 9,
    VideoOutputMplane  = 10,
    SdrCapture         = 11,
    SdrOutput          = 12,
    MetaCapture        = 13,
    MetaOutput         = 14,
    Private            = 0x80,
}

impl BufType {
    pub const fn is_valid(self) -> bool {
        matches!(
            self,
            Self::VideoCapture
                | Self::VideoOutput
                | Self::VideoOverlay
                | Self::VbiCapture
                | Self::VbiOutput
                | Self::SlicedVbiCapture
                | Self::SlicedVbiOutput
                | Self::VideoOutputOverlay
                | Self::VideoCaptureMplane
                | Self::VideoOutputMplane
                | Self::SdrCapture
                | Self::SdrOutput
                | Self::MetaCapture
                | Self::MetaOutput
                | Self::Private
        )
    }

    pub const fn is_multiplanar(self) -> bool {
        matches!(self, Self::VideoCaptureMplane | Self::VideoOutputMplane)
    }

    pub const fn is_output(self) -> bool {
        matches!(
            self,
            Self::VideoOutput
                | Self::VideoOutputMplane
                | Self::VideoOutputOverlay
                | Self::VbiOutput
                | Self::SlicedVbiOutput
                | Self::SdrOutput
                | Self::MetaOutput
        )
    }

    pub const fn is_capture(self) -> bool {
        self.is_valid() && !self.is_output()
    }

    /// 尝试将原始 u32 值转换为 [`BufType`]。
    ///
    /// 若该值不对应任何已知变体，则返回 `None`。
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            1 => Self::VideoCapture,
            2 => Self::VideoOutput,
            3 => Self::VideoOverlay,
            4 => Self::VbiCapture,
            5 => Self::VbiOutput,
            6 => Self::SlicedVbiCapture,
            7 => Self::SlicedVbiOutput,
            8 => Self::VideoOutputOverlay,
            9 => Self::VideoCaptureMplane,
            10 => Self::VideoOutputMplane,
            11 => Self::SdrCapture,
            12 => Self::SdrOutput,
            13 => Self::MetaCapture,
            14 => Self::MetaOutput,
            0x80 => Self::Private,
            _ => return None,
        })
    }
}

/// 缓冲区的内存映射类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Memory {
    Mmap    = 1,
    Userptr = 2,
    Overlay = 3,
    Dmabuf  = 4,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// 内核 timespec — 与 64 位系统上的 `struct __kernel_timespec` 一致（16 字节）。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Timecode {
    pub ty: u32,
    pub flags: u32,
    pub frames: u8,
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub userbits: [u8; 4],
}
