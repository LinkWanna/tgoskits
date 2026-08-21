//! UVC 控件接线：把相机硬件的 Processing Unit / Camera Terminal 控件
//! 映射为 V4L2 控件（CtrlHandler 硬件代理）。
//!
//! 对齐 UVC 1.5 §4.2.2 与 Linux `drivers/media/usb/uvc/uvc_ctrl.c`
//! 的 `uvc_ctrls[]` / `uvc_ctrl_mappings[]`。仅暴露 Linux 已映射为
//! V4L2 CID 的子集（`V4L2_CTRL_TYPE_INTEGER/BOOLEAN/MENU/BUTTON`）；
//! 复合类型（`RECT`/`BITMASK` 偏移分割、PanTilt 8-byte、ROI 10-byte
//! 等）因当前 `v4l2-core` 仅支持标量而暂緩，见 `UVC_CONTROL_*_DEFS` 注释。

use alloc::{boxed::Box, sync::Arc, vec::Vec};

use crab_usb::usb_if::{
    host::ControlSetup,
    transfer::{Recipient, RequestType},
};
use v4l2_core::ctrls::{
    CtrlGetFn, CtrlSetFn,
    class::{CameraClassCtrl, UserClassCtrl},
};

use crate::{
    UvcDevice, UvcHandle,
    descriptors::{ControlCapabilities, RequestCode},
};

/// 解析出的 VC 接口单元（Camera Terminal + Processing Unit）。
#[derive(Debug, Default)]
pub struct VcUnits {
    /// Camera Terminal 的 terminal id 与其 bmControls 位图。
    pub camera_terminal_id: Option<u8>,
    pub camera_controls: Vec<u8>,
    /// Processing Unit 的 unit id 与其 bmControls 位图。
    pub processing_unit_id: Option<u8>,
    pub processing_controls: Vec<u8>,
}

/// 摄像头终端控制选择器 (A.9.4)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum CameraTerminalControl {
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

/// 处理单元控制选择器 (A.9.5)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum ProcessingUnitControl {
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

/// `V4L2_CID_POWER_LINE_FREQUENCY` 菜单项。
const POWER_LINE_FREQ_MENU: &[&str] = &["Disabled", "50 Hz", "60 Hz", "Auto"];

/// `V4L2_CID_EXPOSURE_AUTO` 菜单项。
const EXPOSURE_AUTO_MENU: &[&str] = &[
    "Manual Mode",
    "Aperture Priority Mode",
    "Shutter Priority Mode",
    "Auto Mode",
];

/// UVC 控件的 V4L2 类型
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UvcCtrlType {
    Integer,
    Boolean,
    Menu(&'static [&'static str]),
    Button,
}

/// Processing Unit 控件定义：V4L2 CID ↔ UVC PU (selector, size)。
struct UvcControlDef {
    cid: u32,
    name: &'static str,
    selector: u8,
    size: usize,
    ctrl_bit: u8,
    ty: UvcCtrlType,
}

/// PU 控件映射表（UVC 1.5 Table A-13，Linux `uvc_ctrls[]` PU 部分）
///
/// 仅收录 `size ∈ {1,2,4}` 且在 Linux `uvc_ctrl_mappings[]` 中已映射为 V4L2 的 1:1 项。
const UVC_CONTROL_PU_DEFS: &[UvcControlDef] = &[
    UvcControlDef {
        cid: UserClassCtrl::Brightness as u32,
        name: "Brightness",
        selector: ProcessingUnitControl::Brightness as u8,
        size: 2,
        ctrl_bit: 0,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::Contrast as u32,
        name: "Contrast",
        selector: ProcessingUnitControl::Contrast as u8,
        size: 2,
        ctrl_bit: 1,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::Hue as u32,
        name: "Hue",
        selector: ProcessingUnitControl::Hue as u8,
        size: 2,
        ctrl_bit: 2,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::Saturation as u32,
        name: "Saturation",
        selector: ProcessingUnitControl::Saturation as u8,
        size: 2,
        ctrl_bit: 3,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::Sharpness as u32,
        name: "Sharpness",
        selector: ProcessingUnitControl::Sharpness as u8,
        size: 2,
        ctrl_bit: 4,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::Gamma as u32,
        name: "Gamma",
        selector: ProcessingUnitControl::Gamma as u8,
        size: 2,
        ctrl_bit: 5,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::WhiteBalanceTemperature as u32,
        name: "White Balance Temperature",
        selector: ProcessingUnitControl::WhiteBalanceTemperature as u8,
        size: 2,
        ctrl_bit: 6,
        ty: UvcCtrlType::Integer,
    },
    // White Balance Component (4-byte, Red@0 + Blue@16) 在 Linux 中拆为
    // V4L2_CID_RED_BALANCE / BLUE_BALANCE（offset 0/16），当前标量框架暂緩。
    UvcControlDef {
        cid: UserClassCtrl::BacklightCompensation as u32,
        name: "Backlight Compensation",
        selector: ProcessingUnitControl::BacklightCompensation as u8,
        size: 2,
        ctrl_bit: 8,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::Gain as u32,
        name: "Gain",
        selector: ProcessingUnitControl::Gain as u8,
        size: 2,
        ctrl_bit: 9,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::PowerLineFrequency as u32,
        name: "Power Line Frequency",
        selector: ProcessingUnitControl::PowerLineFrequency as u8,
        size: 1,
        ctrl_bit: 10,
        ty: UvcCtrlType::Menu(POWER_LINE_FREQ_MENU),
    },
    UvcControlDef {
        cid: UserClassCtrl::HueAuto as u32,
        name: "Hue Auto",
        selector: ProcessingUnitControl::HueAuto as u8,
        size: 1,
        ctrl_bit: 11,
        ty: UvcCtrlType::Boolean,
    },
    UvcControlDef {
        cid: UserClassCtrl::AutoWhiteBalance as u32,
        name: "Auto White Balance",
        selector: ProcessingUnitControl::WhiteBalanceTemperatureAuto as u8,
        size: 1,
        ctrl_bit: 12,
        ty: UvcCtrlType::Boolean,
    },
    // White Balance Component Auto (D13, PU 0x0D) 与上同 CID (V4L2_CID_AUTO_WHITE_BALANCE) 设备二选一；
    // Digital Multiplier/Limit、Analog Standard/Lock 在 Linux uvc_ctrl_mappings 中未映射为 V4L2 控制，暂不暴露。
];

/// CT 控件映射表（UVC 1.5 Table A-12，Linux `uvc_ctrls[]` CT 部分）
const UVC_CONTROL_CT_DEFS: &[UvcControlDef] = &[
    UvcControlDef {
        cid: CameraClassCtrl::ExposureAuto as u32,
        name: "Exposure, Auto",
        selector: CameraTerminalControl::AeMode as u8,
        size: 1,
        ctrl_bit: 1,
        ty: UvcCtrlType::Menu(EXPOSURE_AUTO_MENU),
    },
    UvcControlDef {
        cid: CameraClassCtrl::ExposureAutoPriority as u32,
        name: "Exposure, Auto Priority",
        selector: CameraTerminalControl::AePriority as u8,
        size: 1,
        ctrl_bit: 2,
        ty: UvcCtrlType::Boolean,
    },
    UvcControlDef {
        cid: CameraClassCtrl::ExposureAbsolute as u32,
        name: "Exposure (Absolute)",
        selector: CameraTerminalControl::ExposureTimeAbsolute as u8,
        size: 4,
        ctrl_bit: 3,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: CameraClassCtrl::FocusAbsolute as u32,
        name: "Focus (Absolute)",
        selector: CameraTerminalControl::FocusAbsolute as u8,
        size: 2,
        ctrl_bit: 5,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: CameraClassCtrl::FocusAuto as u32,
        name: "Focus, Auto",
        selector: CameraTerminalControl::FocusAuto as u8,
        size: 1,
        ctrl_bit: 17,
        ty: UvcCtrlType::Boolean,
    },
    UvcControlDef {
        cid: CameraClassCtrl::IrisAbsolute as u32,
        name: "Iris, Absolute",
        selector: CameraTerminalControl::IrisAbsolute as u8,
        size: 2,
        ctrl_bit: 7,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: CameraClassCtrl::ZoomAbsolute as u32,
        name: "Zoom, Absolute",
        selector: CameraTerminalControl::ZoomAbsolute as u8,
        size: 2,
        ctrl_bit: 9,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: CameraClassCtrl::Privacy as u32,
        name: "Privacy",
        selector: CameraTerminalControl::Privacy as u8,
        size: 1,
        ctrl_bit: 18,
        ty: UvcCtrlType::Boolean,
    },
    // 以下为 UVC 1.5 存在但因复合/偏移暂緩的项，保留注释以显“已审计”：
    // - CT_ZOOM_RELATIVE (3-byte, Linux custom get/set) → V4L2_CID_ZOOM_CONTINUOUS
    // - CT_PANTILT_ABSOLUTE (8-byte, Pan@0 Tilt@32) → V4L2_CID_PAN_ABSOLUTE / TILT_ABSOLUTE
    // - CT_PANTILT_RELATIVE (4-byte, PanSpeed@0 TiltSpeed@16) → PAN_SPEED/TILT_SPEED
    // - CT_ROLL_ABSOLUTE/C_RELATIVE (2-byte) → V4L2_CID_ROLL_ABSOLUTE 等（当前 CameraClass 未定义）
    // - CT_REGION_OF_INTEREST (10-byte Rect+Bitmask) → V4L2_CID_UVC_ROI_RECT / ROI_AUTO（需 RECT/BITMASK）
    // - CT_EXPOSURE_TIME_RELATIVE (1-byte) / SCANNING_MODE 等在 Linux 未映射为 V4L2
];

/// 检查 bmControls 位图是否置位（UVC 规范：bit N 位于 byte N/8 的位 N%8）。
fn control_supported(bitmap: &[u8], bit: u8) -> bool {
    let byte = (bit / 8) as usize;
    let b = bit % 8;
    bitmap.get(byte).is_some_and(|v| (v >> b) & 1 == 1)
}

/// UVC 控件值解析：1/2/4 字节小端 → i64。
fn decode_uvc_value(buf: &[u8]) -> Option<i64> {
    match buf.len() {
        1 => Some(buf[0] as i64),
        2 => Some(i16::from_le_bytes([buf[0], buf[1]]) as i64),
        4 => Some(i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as i64),
        _ => None,
    }
}

/// UVC 控件值编码：i64 → 1/2/4 字节小端。
fn encode_uvc_value(v: i64, size: usize) -> Option<Vec<u8>> {
    match size {
        1 => Some(vec![v as u8]),
        2 => Some((v as i16).to_le_bytes().to_vec()),
        4 => Some((v as i32).to_le_bytes().to_vec()),
        _ => None,
    }
}

/// 通用控件注册路径：GET_INFO 门控 + 范围读取 + 硬件代理创建。
///
/// 由 PU/CT 各自循环调用，`log_tag` 仅用于日志区分（`\"PU\"`/`\"CT\"`）。
#[allow(clippy::too_many_arguments)]
fn register_control<H: UvcHandle>(
    ctrls: &mut v4l2_core::ctrls::CtrlHandler,
    handle: &Arc<H>,
    vc_iface: u8,
    unit_id: u8,
    bitmap: &[u8],
    def: &UvcControlDef,
    log_tag: &str,
) {
    let cid_raw = def.cid;
    let name = def.name;
    let sel_raw = def.selector;
    let size = def.size;
    let ctrl_bit = def.ctrl_bit;
    let ty = def.ty;
    if ctrls.find(cid_raw).is_some() {
        return;
    }
    if !control_supported(bitmap, ctrl_bit) {
        return;
    }

    // ── GET_INFO 门控：先查 GetInfo 再决定是否发 GetMin/Max ──
    let info_byte = {
        let mut buf = vec![0u8; 1];
        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: RequestCode::GetInfo.into(),
            value: (sel_raw as u16) << 8,
            index: ((unit_id as u16) << 8) | vc_iface as u16,
        };
        match handle.control_in(setup, &mut buf) {
            Ok(_) => buf[0],
            Err(e) => {
                log::debug!("uvc: {log_tag} {name} GetInfo err sel {sel_raw:#x}: {e:?}");
                return;
            }
        }
    };
    let caps = ControlCapabilities::from_bits_truncate(info_byte);
    if caps.contains(ControlCapabilities::DISABLED) {
        log::debug!("uvc: {log_tag} {name} disabled info={info_byte:#x}");
        return;
    }
    if !caps.contains(ControlCapabilities::GET) {
        log::debug!("uvc: {log_tag} {name} no GET support info={info_byte:#x}");
        return;
    }

    let read = {
        let handle = handle.clone();
        move |request: RequestCode| -> Option<i64> {
            let mut buf = vec![0u8; size];
            let setup = ControlSetup {
                request_type: RequestType::Class,
                recipient: Recipient::Interface,
                request: request.into(),
                value: (sel_raw as u16) << 8,
                index: ((unit_id as u16) << 8) | vc_iface as u16,
            };
            handle.control_in(setup, &mut buf).ok()?;
            decode_uvc_value(&buf)
        }
    };

    let h = handle.clone();
    let get_fn: CtrlGetFn = Box::new(move || {
        let mut buf = vec![0u8; size];
        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: RequestCode::GetCur.into(),
            value: (sel_raw as u16) << 8,
            index: ((unit_id as u16) << 8) | vc_iface as u16,
        };
        h.control_in(setup, &mut buf)
            .map_err(|_e| v4l2_core::V4l2Error::Io)?;
        let raw = decode_uvc_value(&buf).ok_or(v4l2_core::V4l2Error::Io)?;
        if cid_raw == CameraClassCtrl::ExposureAuto as u32 {
            Ok(raw.trailing_zeros() as i64)
        } else {
            Ok(raw)
        }
    });

    let h = handle.clone();
    let set_fn: CtrlSetFn = Box::new(move |v| {
        let v = if cid_raw == CameraClassCtrl::ExposureAuto as u32 {
            1i64 << v
        } else {
            v
        };
        let buf = encode_uvc_value(v, size).ok_or(v4l2_core::V4l2Error::Io)?;
        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: RequestCode::SetCur.into(),
            value: (sel_raw as u16) << 8,
            index: ((unit_id as u16) << 8) | vc_iface as u16,
        };
        h.control_out(setup, &buf)
            .map_err(|_| v4l2_core::V4l2Error::Io)?;
        Ok(v)
    });

    let ops = v4l2_core::ctrls::CtrlOps {
        get: Some(get_fn),
        try_ctrl: None,
        set: set_fn,
    };

    let res = match ty {
        UvcCtrlType::Integer => {
            let Some(min) = read(RequestCode::GetMin) else {
                return;
            };
            let Some(max) = read(RequestCode::GetMax) else {
                return;
            };
            let step = read(RequestCode::GetRes).unwrap_or(1).max(1);
            let default = read(RequestCode::GetDef).unwrap_or(min);
            ctrls.new_int(cid_raw, name, min, max, step, default, Some(ops))
        }
        UvcCtrlType::Boolean => {
            let default = read(RequestCode::GetDef).unwrap_or(0);
            ctrls.new_bool(cid_raw, name, default != 0, Some(ops))
        }
        UvcCtrlType::Menu(qmenu) => {
            let default = read(RequestCode::GetDef).unwrap_or(0);
            let default_idx = if cid_raw == CameraClassCtrl::ExposureAuto as u32 {
                (default.trailing_zeros() as i64).clamp(0, qmenu.len() as i64 - 1) as u32
            } else {
                (default as u32).min(qmenu.len() as u32 - 1)
            };
            ctrls.new_menu(
                cid_raw,
                name,
                qmenu.len() as u32,
                default_idx,
                qmenu,
                Some(ops),
            )
        }
        UvcCtrlType::Button => ctrls.new_button(cid_raw, name, Some(ops)),
    };
    if let Err(e) = res {
        log::warn!("uvc: skip {log_tag} {name} (0x{cid_raw:08x}): {e:?}");
    }
}

impl<H: UvcHandle> UvcDevice<H> {
    pub(crate) fn register_controls(&mut self, units: &VcUnits) {
        let vc_iface = self.vc_iface_num;
        if let Some(unit_id) = units.processing_unit_id {
            for def in UVC_CONTROL_PU_DEFS {
                register_control(
                    &mut self.ctrls,
                    &self.handle,
                    vc_iface,
                    unit_id,
                    &units.processing_controls,
                    def,
                    "PU",
                );
            }
        }
        if let Some(unit_id) = units.camera_terminal_id {
            for def in UVC_CONTROL_CT_DEFS {
                register_control(
                    &mut self.ctrls,
                    &self.handle,
                    vc_iface,
                    unit_id,
                    &units.camera_controls,
                    def,
                    "CT",
                );
            }
        }
    }
}
