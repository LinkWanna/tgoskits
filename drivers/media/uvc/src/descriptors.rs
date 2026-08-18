use alloc::vec::Vec;

use anyhow::anyhow;
use bitflags::bitflags;
use crab_usb::err::USBError;
use log::trace;

/// UVC 类特定请求码 (A.8)——互斥值，用枚举表达。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RequestCode {
    SetCur  = 0x01,
    GetCur  = 0x81,
    GetMin  = 0x82,
    GetMax  = 0x83,
    GetRes  = 0x84,
    GetLen  = 0x85,
    GetInfo = 0x86,
    GetDef  = 0x87,
}

/// UVC 接口子类代码 (A.2)——互斥值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InterfaceSubclass {
    Undefined      = 0x00,
    VideoControl   = 0x01,
    VideoStreaming = 0x02,
    VideoInterfaceCollection = 0x03,
}

/// UVC 协议代码 (A.3)
pub mod protocol_codes {
    pub const UNDEFINED: u8 = 0x00;
}

/// VideoControl 接口描述符子类型 (A.5)——互斥值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VcDescriptorSubtype {
    Undefined      = 0x00,
    Header         = 0x01,
    InputTerminal  = 0x02,
    OutputTerminal = 0x03,
    SelectorUnit   = 0x04,
    ProcessingUnit = 0x05,
    ExtensionUnit  = 0x06,
}

/// VideoStreaming 接口描述符子类型 (A.6)——互斥值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VsDescriptorSubtype {
    Undefined           = 0x00,
    InputHeader         = 0x01,
    OutputHeader        = 0x02,
    StillImageFrame     = 0x03,
    FormatUncompressed  = 0x04,
    FrameUncompressed   = 0x05,
    FormatMjpeg         = 0x06,
    FrameMjpeg          = 0x07,
    FormatMpeg2Ts       = 0x0A,
    FormatDv            = 0x0C,
    Colorformat         = 0x0D,
    FormatFrameBased    = 0x10,
    FrameFrameBased     = 0x11,
    FormatStreamBased   = 0x12,
    FormatH264          = 0x13,
    FrameH264           = 0x14,
    FormatH264Simulcast = 0x15,
}

/// UVC 描述符类型——互斥值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DescriptorType {
    Undefined     = 0x00,
    Device        = 0x01,
    Configuration = 0x02,
    String        = 0x03,
    Interface     = 0x04,
    Endpoint      = 0x05,
    CsInterface   = 0x24,
    CsEndpoint    = 0x25,
}

/// 摄像头终端控制选择器 (A.9.4)——互斥值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CameraTerminalControl {
    Undefined            = 0x00,
    ScanningMode         = 0x01,
    AeMode               = 0x02,
    AePriority           = 0x03,
    ExposureTimeAbsolute = 0x04,
    ExposureTimeRelative = 0x05,
    FocusAbsolute        = 0x06,
    FocusRelative        = 0x07,
    FocusAuto            = 0x08,
    IrisAbsolute         = 0x09,
    IrisRelative         = 0x0A,
    ZoomAbsolute         = 0x0B,
    ZoomRelative         = 0x0C,
    PantiltAbsolute      = 0x0D,
    PantiltRelative      = 0x0E,
    RollAbsolute         = 0x0F,
    RollRelative         = 0x10,
    Privacy              = 0x11,
    FocusSimple          = 0x12,
    DigitalWindow        = 0x13,
    RegionOfInterest     = 0x14,
}

/// 处理单元控制选择器 (A.9.5)——互斥值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProcessingUnitControl {
    Undefined           = 0x00,
    BacklightCompensation = 0x01,
    Brightness          = 0x02,
    Contrast            = 0x03,
    Gain                = 0x04,
    PowerLineFrequency  = 0x05,
    Hue                 = 0x06,
    Saturation          = 0x07,
    Sharpness           = 0x08,
    Gamma               = 0x09,
    WhiteBalanceTemperature = 0x0A,
    WhiteBalanceTemperatureAuto = 0x0B,
    WhiteBalanceComponent = 0x0C,
    WhiteBalanceComponentAuto = 0x0D,
    DigitalMultiplier   = 0x0E,
    DigitalMultiplierLimit = 0x0F,
    HueAuto             = 0x10,
    AnalogVideoStandard = 0x11,
    AnalogLockStatus    = 0x12,
    ContrastAuto        = 0x13,
}

/// VideoStreaming 接口控制选择器 (A.9.7)——互斥值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VideoStreamingControl {
    Undefined          = 0x00,
    Probe              = 0x01,
    Commit             = 0x02,
    StillProbe         = 0x03,
    StillCommit        = 0x04,
    StillImageTrigger  = 0x05,
    StreamErrorCode    = 0x06,
    GenerateKeyFrame   = 0x07,
    UpdateFrameSegment = 0x08,
    SyncDelay          = 0x09,
}

/// 终端类型 (B.1-B.4)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum TerminalType {
    // USB 终端类型 (B.1)
    TtVendorSpecific   = 0x0100,
    TtStreaming        = 0x0101,
    // 输入终端类型 (B.2)
    IttVendorSpecific  = 0x0200,
    IttCamera          = 0x0201,
    IttMediaTransportInput = 0x0202,
    // 输出终端类型 (B.3)
    OttVendorSpecific  = 0x0300,
    OttDisplay         = 0x0301,
    OttMediaTransportOutput = 0x0302,
    // 外部终端类型 (B.4)
    ExternalVendorSpecific = 0x0400,
    CompositeConnector = 0x0401,
    SvideoConnector    = 0x0402,
    ComponentConnector = 0x0403,
}

/// UVC格式GUID常量
pub mod format_guids {
    // YUY2 格式 GUID
    pub const YUY2: [u8; 16] = [
        0x59, 0x55, 0x59, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];

    // NV12 格式 GUID
    pub const NV12: [u8; 16] = [
        0x4e, 0x56, 0x31, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];

    // RGB24 格式 GUID (RGB3)
    pub const RGB24: [u8; 16] = [
        0x52, 0x47, 0x42, 0x33, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];

    // UYVY 格式 GUID
    pub const UYVY: [u8; 16] = [
        0x55, 0x59, 0x56, 0x59, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];

    // BGR24 格式 GUID (BGR3)
    pub const BGR24: [u8; 16] = [
        0x42, 0x47, 0x52, 0x33, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
}

bitflags! {
    /// 载荷头标志 (2.4.3.3)。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PayloadHeaderFlags: u8 {
        const EOH = 1 << 7; // End of Header
        const ERR = 1 << 6; // Error
        const STI = 1 << 5; // Still Image
        const RES = 1 << 4; // Reserved
        const SCR = 1 << 3; // Source Clock Reference
        const PTS = 1 << 2; // Presentation Time Stamp
        const EOF = 1 << 1; // End of Frame
        const FID = 1 << 0; // Frame ID
    }
}

bitflags! {
    /// 控制能力标志 (4.1.2)。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ControlCapabilities: u8 {
        const GET = 1 << 0;
        const SET = 1 << 1;
        const DISABLED = 1 << 2;
        const AUTOUPDATE = 1 << 3;
        const ASYNCHRONOUS = 1 << 4;
    }
}

/// 原始值 → 枚举（`match x.into()` scrutinee 用；非法值兜底到 `Undefined`，
/// 落入 match 的 `_` 分支——UVC 规范 0x00 即 UNDEFINED）。
impl From<u8> for InterfaceSubclass {
    fn from(v: u8) -> Self {
        match v {
            0x01 => Self::VideoControl,
            0x02 => Self::VideoStreaming,
            0x03 => Self::VideoInterfaceCollection,
            _ => Self::Undefined,
        }
    }
}

impl From<u8> for DescriptorType {
    fn from(v: u8) -> Self {
        match v {
            0x01 => Self::Device,
            0x02 => Self::Configuration,
            0x03 => Self::String,
            0x04 => Self::Interface,
            0x05 => Self::Endpoint,
            0x24 => Self::CsInterface,
            0x25 => Self::CsEndpoint,
            _ => Self::Undefined,
        }
    }
}

impl From<u8> for VcDescriptorSubtype {
    fn from(v: u8) -> Self {
        match v {
            0x01 => Self::Header,
            0x02 => Self::InputTerminal,
            0x03 => Self::OutputTerminal,
            0x04 => Self::SelectorUnit,
            0x05 => Self::ProcessingUnit,
            0x06 => Self::ExtensionUnit,
            _ => Self::Undefined,
        }
    }
}

impl From<u8> for VsDescriptorSubtype {
    fn from(v: u8) -> Self {
        match v {
            0x01 => Self::InputHeader,
            0x02 => Self::OutputHeader,
            0x03 => Self::StillImageFrame,
            0x04 => Self::FormatUncompressed,
            0x05 => Self::FrameUncompressed,
            0x06 => Self::FormatMjpeg,
            0x07 => Self::FrameMjpeg,
            0x0A => Self::FormatMpeg2Ts,
            0x0C => Self::FormatDv,
            0x0D => Self::Colorformat,
            0x10 => Self::FormatFrameBased,
            0x11 => Self::FrameFrameBased,
            0x12 => Self::FormatStreamBased,
            0x13 => Self::FormatH264,
            0x14 => Self::FrameH264,
            0x15 => Self::FormatH264Simulcast,
            _ => Self::Undefined,
        }
    }
}

/// 枚举 → 原始值转换（`into()` 用，类型安全；与 `as u8` 等价但显式）。
impl From<RequestCode> for u8 {
    fn from(v: RequestCode) -> u8 {
        v as u8
    }
}

impl From<InterfaceSubclass> for u8 {
    fn from(v: InterfaceSubclass) -> u8 {
        v as u8
    }
}

impl From<DescriptorType> for u8 {
    fn from(v: DescriptorType) -> u8 {
        v as u8
    }
}

impl From<VideoStreamingControl> for u8 {
    fn from(v: VideoStreamingControl) -> u8 {
        v as u8
    }
}

impl From<TerminalType> for u16 {
    fn from(v: TerminalType) -> u16 {
        v as u16
    }
}

/// UVC 请求码 → USB 传输请求（`Request: From<u8>`，一步转换）。
impl From<RequestCode> for crab_usb::usb_if::transfer::Request {
    fn from(v: RequestCode) -> Self {
        (v as u8).into()
    }
}

/// UVC描述符解析器
pub struct DescriptorParser;

impl DescriptorParser {
    /// 创建新的描述符解析器实例
    pub fn new() -> Self {
        Self
    }

    /// 解析VideoControl头描述符
    pub fn parse_vc_header(&self, data: &[u8]) -> Result<VcHeaderDescriptor, USBError> {
        if data.len() < 12 {
            Err(anyhow!("VC header descriptor too short"))?;
        }

        let length = data[0] as usize;
        let descriptor_type = data[1];
        let descriptor_subtype = data[2];

        if descriptor_type != DescriptorType::CsInterface as u8
            || descriptor_subtype != VcDescriptorSubtype::Header as u8
        {
            Err(anyhow!("Not a VC header descriptor"))?;
        }

        let bcd_uvc = u16::from_le_bytes([data[3], data[4]]);
        let total_length = u16::from_le_bytes([data[5], data[6]]);
        let clock_frequency = u32::from_le_bytes([data[7], data[8], data[9], data[10]]);
        let in_collection = data[11];

        trace!(
            "VC Header: UVC {}.{}, total_len={}, clock={} Hz, interfaces={}",
            bcd_uvc >> 8,
            bcd_uvc & 0xff,
            total_length,
            clock_frequency,
            in_collection
        );

        Ok(VcHeaderDescriptor {
            length,
            bcd_uvc,
            total_length,
            clock_frequency,
            in_collection,
        })
    }

    /// 解析输入终端描述符
    pub fn parse_input_terminal(&self, data: &[u8]) -> Result<InputTerminalDescriptor, USBError> {
        if data.len() < 15 {
            Err(anyhow!("Input terminal descriptor too short"))?;
        }

        let length = data[0] as usize;
        let terminal_id = data[3];
        let terminal_type = u16::from_le_bytes([data[4], data[5]]);
        let associated_terminal = data[6];

        trace!(
            "Input Terminal: ID={terminal_id}, type=0x{terminal_type:04x}, \
             associated={associated_terminal}"
        );

        // 摄像头终端有额外字段
        if terminal_type == TerminalType::IttCamera.into() && length >= 18 {
            let objective_focal_length_min = u16::from_le_bytes([data[8], data[9]]);
            let objective_focal_length_max = u16::from_le_bytes([data[10], data[11]]);
            let ocular_focal_length = u16::from_le_bytes([data[12], data[13]]);
            let controls_size = data[14] as usize;

            let controls = if length >= 15 + controls_size {
                data[15..15 + controls_size].to_vec()
            } else {
                vec![]
            };

            Ok(InputTerminalDescriptor::Camera {
                length,
                terminal_id,
                terminal_type,
                associated_terminal,
                objective_focal_length_min,
                objective_focal_length_max,
                ocular_focal_length,
                controls,
            })
        } else {
            Ok(InputTerminalDescriptor::Generic {
                length,
                terminal_id,
                terminal_type,
                associated_terminal,
            })
        }
    }

    /// 解析处理单元描述符
    pub fn parse_processing_unit(&self, data: &[u8]) -> Result<ProcessingUnitDescriptor, USBError> {
        if data.len() < 10 {
            Err(anyhow!("Processing unit descriptor too short"))?;
        }

        let length = data[0] as usize;
        let unit_id = data[3];
        let source_id = data[4];
        let max_multiplier = u16::from_le_bytes([data[5], data[6]]);
        let controls_size = data[7] as usize;

        if length < 8 + controls_size {
            Err(anyhow!("Processing unit controls data incomplete"))?;
        }

        let controls = data[8..8 + controls_size].to_vec();

        trace!(
            "Processing Unit: ID={unit_id}, source={source_id}, max_mult={max_multiplier}, \
             controls={controls:02x?}"
        );

        Ok(ProcessingUnitDescriptor {
            length,
            unit_id,
            source_id,
            max_multiplier,
            controls,
        })
    }

    /// 解析VideoStreaming输入头描述符
    pub fn parse_vs_input_header(&self, data: &[u8]) -> Result<VsInputHeaderDescriptor, USBError> {
        if data.len() < 13 {
            Err(anyhow!("VS input header descriptor too short"))?;
        }

        let length = data[0] as usize;
        let num_formats = data[3];
        let total_length = u16::from_le_bytes([data[4], data[5]]);
        let endpoint_address = data[6];
        let info = data[7];
        let terminal_link = data[8];
        let still_capture_method = data[9];
        let trigger_support = data[10];
        let trigger_usage = data[11];
        let controls_size = data[12] as usize;

        if length < 13 + controls_size * num_formats as usize {
            Err(anyhow!("VS input header format controls data incomplete"))?;
        }

        let format_controls = data[13..13 + controls_size * num_formats as usize].to_vec();

        trace!(
            "VS Input Header: formats={num_formats}, total_len={total_length}, \
             endpoint=0x{endpoint_address:02x}, terminal={terminal_link}"
        );

        Ok(VsInputHeaderDescriptor {
            length,
            num_formats,
            total_length,
            endpoint_address,
            info,
            terminal_link,
            still_capture_method,
            trigger_support,
            trigger_usage,
            format_controls,
        })
    }

    /// 解析未压缩格式描述符
    pub fn parse_uncompressed_format(
        &self,
        data: &[u8],
    ) -> Result<UncompressedFormatDescriptor, USBError> {
        if data.len() < 27 {
            Err(anyhow!("Uncompressed format descriptor too short"))?;
        }

        let length = data[0] as usize;
        let format_index = data[3];
        let num_frame_descriptors = data[4];
        let mut guid = [0u8; 16];
        guid.copy_from_slice(&data[5..21]);
        let bits_per_pixel = data[21];
        let default_frame_index = data[22];
        let aspect_ratio_x = data[23];
        let aspect_ratio_y = data[24];
        let interlace_flags = data[25];
        let copy_protect = data[26];

        trace!(
            "Uncompressed Format: index={format_index}, frames={num_frame_descriptors}, \
             GUID={guid:02x?}, bpp={bits_per_pixel}"
        );

        Ok(UncompressedFormatDescriptor {
            length,
            format_index,
            num_frame_descriptors,
            guid,
            bits_per_pixel,
            default_frame_index,
            aspect_ratio_x,
            aspect_ratio_y,
            interlace_flags,
            copy_protect,
        })
    }

    /// 解析MJPEG格式描述符
    pub fn parse_mjpeg_format(&self, data: &[u8]) -> Result<MjpegFormatDescriptor, USBError> {
        if data.len() < 11 {
            Err(anyhow!("MJPEG format descriptor too short"))?;
        }

        let length = data[0] as usize;
        let format_index = data[3];
        let num_frame_descriptors = data[4];
        let flags = data[5];
        let default_frame_index = data[6];
        let aspect_ratio_x = data[7];
        let aspect_ratio_y = data[8];
        let interlace_flags = data[9];
        let copy_protect = data[10];

        trace!(
            "MJPEG Format: index={format_index}, frames={num_frame_descriptors}, \
             flags=0x{flags:02x}"
        );

        Ok(MjpegFormatDescriptor {
            length,
            format_index,
            num_frame_descriptors,
            flags,
            default_frame_index,
            aspect_ratio_x,
            aspect_ratio_y,
            interlace_flags,
            copy_protect,
        })
    }

    /// 解析帧描述符
    pub fn parse_frame_descriptor(&self, data: &[u8]) -> Result<FrameDescriptor, USBError> {
        if data.len() < 26 {
            Err(anyhow!("Frame descriptor too short"))?;
        }

        let length = data[0] as usize;
        let frame_index = data[3];
        let capabilities = data[4];
        let width = u16::from_le_bytes([data[5], data[6]]);
        let height = u16::from_le_bytes([data[7], data[8]]);
        let min_bit_rate = u32::from_le_bytes([data[9], data[10], data[11], data[12]]);
        let max_bit_rate = u32::from_le_bytes([data[13], data[14], data[15], data[16]]);
        let max_video_frame_buffer_size =
            u32::from_le_bytes([data[17], data[18], data[19], data[20]]);
        let default_frame_interval = u32::from_le_bytes([data[21], data[22], data[23], data[24]]);
        let frame_interval_type = data[25];

        trace!(
            "Frame: {width}x{height}, bitrate={min_bit_rate}-{max_bit_rate}, \
             buffer_size={max_video_frame_buffer_size}, interval={default_frame_interval}, \
             type={frame_interval_type}"
        );

        // 解析帧间隔数据
        let mut frame_intervals = Vec::new();
        let mut pos = 26;

        match frame_interval_type {
            0 if length >= pos + 12 => {
                // 连续帧间隔
                let min_frame_interval =
                    u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
                let max_frame_interval = u32::from_le_bytes([
                    data[pos + 4],
                    data[pos + 5],
                    data[pos + 6],
                    data[pos + 7],
                ]);
                let step_frame_interval = u32::from_le_bytes([
                    data[pos + 8],
                    data[pos + 9],
                    data[pos + 10],
                    data[pos + 11],
                ]);

                frame_intervals = vec![min_frame_interval, max_frame_interval, step_frame_interval];
            }
            n if n > 0 => {
                // 离散帧间隔
                for _ in 0..n {
                    if pos + 4 <= length {
                        let interval = u32::from_le_bytes([
                            data[pos],
                            data[pos + 1],
                            data[pos + 2],
                            data[pos + 3],
                        ]);
                        frame_intervals.push(interval);
                        pos += 4;
                    }
                }
            }
            _ => {}
        }

        Ok(FrameDescriptor {
            length,
            frame_index,
            capabilities,
            width,
            height,
            min_bit_rate,
            max_bit_rate,
            max_video_frame_buffer_size,
            default_frame_interval,
            frame_interval_type,
            frame_intervals,
        })
    }

    /// 计算帧率（从帧间隔）
    pub fn interval_to_fps(interval: u32) -> u32 {
        10_000_000u32.checked_div(interval).unwrap_or(0) // 100ns单位转换为fps
    }

    /// 计算帧间隔（从帧率）
    pub fn fps_to_interval(fps: u32) -> u32 {
        10_000_000u32.checked_div(fps).unwrap_or(0) // fps转换为100ns单位
    }
}

/// VideoControl头描述符
#[derive(Debug, Clone)]
pub struct VcHeaderDescriptor {
    pub length: usize,
    pub bcd_uvc: u16,
    pub total_length: u16,
    pub clock_frequency: u32,
    pub in_collection: u8,
}

/// 输入终端描述符
#[derive(Debug, Clone)]
pub enum InputTerminalDescriptor {
    Camera {
        length: usize,
        terminal_id: u8,
        terminal_type: u16,
        associated_terminal: u8,
        objective_focal_length_min: u16,
        objective_focal_length_max: u16,
        ocular_focal_length: u16,
        controls: Vec<u8>,
    },
    Generic {
        length: usize,
        terminal_id: u8,
        terminal_type: u16,
        associated_terminal: u8,
    },
}

/// 处理单元描述符
#[derive(Debug, Clone)]
pub struct ProcessingUnitDescriptor {
    pub length: usize,
    pub unit_id: u8,
    pub source_id: u8,
    pub max_multiplier: u16,
    pub controls: Vec<u8>,
}

/// VideoStreaming 输入头描述符
#[derive(Debug, Clone)]
pub struct VsInputHeaderDescriptor {
    pub length: usize,
    pub num_formats: u8,
    pub total_length: u16,
    pub endpoint_address: u8,
    pub info: u8,
    pub terminal_link: u8,
    pub still_capture_method: u8,
    pub trigger_support: u8,
    pub trigger_usage: u8,
    pub format_controls: Vec<u8>,
}

/// 未压缩格式描述符
#[derive(Debug, Clone)]
pub struct UncompressedFormatDescriptor {
    pub length: usize,
    pub format_index: u8,
    pub num_frame_descriptors: u8,
    pub guid: [u8; 16],
    pub bits_per_pixel: u8,
    pub default_frame_index: u8,
    pub aspect_ratio_x: u8,
    pub aspect_ratio_y: u8,
    pub interlace_flags: u8,
    pub copy_protect: u8,
}

/// MJPEG 格式描述符
#[derive(Debug, Clone)]
pub struct MjpegFormatDescriptor {
    pub length: usize,
    pub format_index: u8,
    pub num_frame_descriptors: u8,
    pub flags: u8,
    pub default_frame_index: u8,
    pub aspect_ratio_x: u8,
    pub aspect_ratio_y: u8,
    pub interlace_flags: u8,
    pub copy_protect: u8,
}

/// 帧描述符
#[derive(Debug, Clone)]
pub struct FrameDescriptor {
    pub length: usize,    // 描述符长度
    pub frame_index: u8,  // 帧索引
    pub capabilities: u8, // 帧能力标志
    pub width: u16,
    pub height: u16,
    pub min_bit_rate: u32,
    pub max_bit_rate: u32,
    pub max_video_frame_buffer_size: u32,
    pub default_frame_interval: u32,
    pub frame_interval_type: u8,   // 帧间隔类型
    pub frame_intervals: Vec<u32>, // 帧间隔列表
}

impl Default for DescriptorParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fps_conversion() {
        // 测试30fps
        let interval_30fps = 333333; // 100ns单位
        assert_eq!(DescriptorParser::interval_to_fps(interval_30fps), 30);
        assert_eq!(DescriptorParser::fps_to_interval(30), 333333);

        // 测试60fps
        let interval_60fps = 166666;
        assert_eq!(DescriptorParser::interval_to_fps(interval_60fps), 60);
        assert_eq!(DescriptorParser::fps_to_interval(60), 166666);
    }

    #[test]
    fn test_guid_constants() {
        // 确保GUID常量正确定义
        assert_eq!(format_guids::YUY2[0..4], [0x59, 0x55, 0x59, 0x32]);
        assert_eq!(format_guids::NV12[0..4], [0x4e, 0x56, 0x31, 0x32]);
        assert_eq!(format_guids::RGB24[0..4], [0x52, 0x47, 0x42, 0x33]);
    }
}
