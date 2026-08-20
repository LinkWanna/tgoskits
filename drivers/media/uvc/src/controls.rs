//! UVC 控件接线：把相机硬件的 Processing Unit / Camera Terminal 控件
//! 映射为 V4L2 控件（CtrlHandler 硬件代理）。
//!
//! 对齐 Linux uvcvideo 的 uvc_ctrl.c 映射表（V4L2 CID ↔ UVC 选择器）。

use alloc::{boxed::Box, vec::Vec};

use v4l2_core::{
    ctrls::{
        CtrlConfig, CtrlGetFn, CtrlHandler, CtrlOps, CtrlSetFn, CtrlType,
        class::{CameraClassCtrl, UserClassCtrl},
    },
    interface::ctrl::CtrlFlags,
};

use crate::{
    UvcHandle,
    descriptors::{CameraTerminalControl, ProcessingUnitControl, RequestCode},
};

/// `V4L2_CID_POWER_LINE_FREQUENCY` 菜单项。
const POWER_LINE_FREQ_MENU: &[&str] = &["Disabled", "50 Hz", "60 Hz", "Auto"];

/// `V4L2_CID_EXPOSURE_AUTO` 菜单项。
const EXPOSURE_AUTO_MENU: &[&str] = &[
    "Manual Mode",
    "Aperture Priority Mode",
    "Shutter Priority Mode",
    "Auto Mode",
];

/// 控件所在单元（Processing Unit 或 Camera Terminal）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    Processing,
    CameraTerminal,
}

/// UVC 控件定义：V4L2 CID ↔ UVC (unit, selector, size)。
pub struct UvcControlDef {
    pub cid: u32,
    pub name: &'static str,
    pub unit_kind: UnitKind,
    pub selector: u8,
    /// 值字节数（1/2/4）。
    pub size: usize,
    /// bmControls 位图中的 bit（= selector - 1）。
    pub ctrl_bit: u8,
    /// 布尔控件（值 0/1）。
    pub is_bool: bool,
    /// 菜单控件（菜单项数；非菜单为 0）。
    pub menu_items: u32,
}

/// V4L2 → UVC 控件映射表（对齐 Linux uvc_ctrl.c 常用项）。
pub const UVC_CONTROL_DEFS: &[UvcControlDef] = &[
    UvcControlDef {
        cid: UserClassCtrl::BacklightCompensation as u32,
        name: "Backlight Compensation",
        unit_kind: UnitKind::Processing,
        selector: ProcessingUnitControl::BacklightCompensation as u8,
        size: 2,
        ctrl_bit: 0,
        is_bool: false,
        menu_items: 0,
    },
    UvcControlDef {
        cid: UserClassCtrl::Brightness as u32,
        name: "Brightness",
        unit_kind: UnitKind::Processing,
        selector: ProcessingUnitControl::Brightness as u8,
        size: 2,
        ctrl_bit: 1,
        is_bool: false,
        menu_items: 0,
    },
    UvcControlDef {
        cid: UserClassCtrl::Contrast as u32,
        name: "Contrast",
        unit_kind: UnitKind::Processing,
        selector: ProcessingUnitControl::Contrast as u8,
        size: 2,
        ctrl_bit: 2,
        is_bool: false,
        menu_items: 0,
    },
    UvcControlDef {
        cid: UserClassCtrl::Gain as u32,
        name: "Gain",
        unit_kind: UnitKind::Processing,
        selector: ProcessingUnitControl::Gain as u8,
        size: 2,
        ctrl_bit: 3,
        is_bool: false,
        menu_items: 0,
    },
    UvcControlDef {
        cid: UserClassCtrl::PowerLineFrequency as u32,
        name: "Power Line Frequency",
        unit_kind: UnitKind::Processing,
        selector: ProcessingUnitControl::PowerLineFrequency as u8,
        size: 1,
        ctrl_bit: 4,
        is_bool: false,
        menu_items: 4, // Disabled / 50Hz / 60Hz / Auto
    },
    UvcControlDef {
        cid: UserClassCtrl::Hue as u32,
        name: "Hue",
        unit_kind: UnitKind::Processing,
        selector: ProcessingUnitControl::Hue as u8,
        size: 2,
        ctrl_bit: 5,
        is_bool: false,
        menu_items: 0,
    },
    UvcControlDef {
        cid: UserClassCtrl::Saturation as u32,
        name: "Saturation",
        unit_kind: UnitKind::Processing,
        selector: ProcessingUnitControl::Saturation as u8,
        size: 2,
        ctrl_bit: 6,
        is_bool: false,
        menu_items: 0,
    },
    UvcControlDef {
        cid: UserClassCtrl::Sharpness as u32,
        name: "Sharpness",
        unit_kind: UnitKind::Processing,
        selector: ProcessingUnitControl::Sharpness as u8,
        size: 2,
        ctrl_bit: 7,
        is_bool: false,
        menu_items: 0,
    },
    UvcControlDef {
        cid: UserClassCtrl::Gamma as u32,
        name: "Gamma",
        unit_kind: UnitKind::Processing,
        selector: ProcessingUnitControl::Gamma as u8,
        size: 2,
        ctrl_bit: 8,
        is_bool: false,
        menu_items: 0,
    },
    UvcControlDef {
        cid: UserClassCtrl::WhiteBalanceTemperature as u32,
        name: "White Balance Temperature",
        unit_kind: UnitKind::Processing,
        selector: ProcessingUnitControl::WhiteBalanceTemperature as u8,
        size: 2,
        ctrl_bit: 9,
        is_bool: false,
        menu_items: 0,
    },
    UvcControlDef {
        cid: UserClassCtrl::AutoWhiteBalance as u32,
        name: "Auto White Balance",
        unit_kind: UnitKind::Processing,
        selector: ProcessingUnitControl::WhiteBalanceTemperatureAuto as u8,
        size: 1,
        ctrl_bit: 10,
        is_bool: true,
        menu_items: 0,
    },
    UvcControlDef {
        cid: UserClassCtrl::HueAuto as u32,
        name: "Hue Auto",
        unit_kind: UnitKind::Processing,
        selector: ProcessingUnitControl::HueAuto as u8,
        size: 1,
        ctrl_bit: 15,
        is_bool: true,
        menu_items: 0,
    },
    UvcControlDef {
        cid: CameraClassCtrl::ExposureAuto as u32,
        name: "Exposure, Auto",
        unit_kind: UnitKind::CameraTerminal,
        selector: CameraTerminalControl::AeMode as u8,
        size: 1,
        ctrl_bit: 1,
        is_bool: false,
        menu_items: 4, // Manual / Aperture / Shutter / Auto
    },
    UvcControlDef {
        cid: CameraClassCtrl::ExposureAbsolute as u32,
        name: "Exposure (Absolute)",
        unit_kind: UnitKind::CameraTerminal,
        selector: CameraTerminalControl::ExposureTimeAbsolute as u8,
        size: 4,
        ctrl_bit: 3,
        is_bool: false,
        menu_items: 0,
    },
];

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

/// 检查 bmControls 位图是否置位（UVC 规范：bit N 位于 byte N/8 的位 N%8）。
pub fn control_supported(bitmap: &[u8], bit: u8) -> bool {
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

/// 将支持范围内的 UVC 控件注册进 CtrlHandler（硬件代理）。
///
/// `get_fn`/`set_fn` 闭包捕获 `handle`（Arc）与控件坐标，
/// 每次 G_CTRL/S_CTRL 都走 USB GET_CUR/SET_CUR。
pub fn register_uvc_controls<H: UvcHandle>(
    ctrls: &mut CtrlHandler,
    handle: &alloc::sync::Arc<H>,
    vc_iface: u8,
    units: &VcUnits,
) {
    for def in UVC_CONTROL_DEFS {
        // 1. 检查该 unit 是否声明支持此控件
        let (unit_id, supported) = match def.unit_kind {
            UnitKind::Processing => (
                units.processing_unit_id,
                units
                    .processing_unit_id
                    .is_some_and(|_| control_supported(&units.processing_controls, def.ctrl_bit)),
            ),
            UnitKind::CameraTerminal => (
                units.camera_terminal_id,
                units
                    .camera_terminal_id
                    .is_some_and(|_| control_supported(&units.camera_controls, def.ctrl_bit)),
            ),
        };
        let Some(unit_id) = unit_id else { continue };
        if !supported {
            continue;
        }

        // 2. 查询范围（min/max/step/default 一次拿齐）
        //    min/max 查询失败或设备不支持时跳过该控件；
        //    step/default 失败按 Linux 语义降级（step=1、default=min）。
        let read = |request| -> Option<i64> {
            let mut buf = vec![0u8; def.size];
            crate::UvcDevice::get_pu_control(
                handle.as_ref(),
                vc_iface,
                unit_id,
                def.selector,
                request,
                &mut buf,
            )
            .ok()?;
            decode_uvc_value(&buf)
        };
        let Some(min) = read(RequestCode::GetMin) else {
            continue;
        };
        let Some(max) = read(RequestCode::GetMax) else {
            continue;
        };
        let step = read(RequestCode::GetRes).unwrap_or(1).max(1);
        let default = read(RequestCode::GetDef).unwrap_or(min);

        // 3. 构造 get/set 闭包（捕获 Arc<H> 与控件坐标，走同步控制传输）
        let h = handle.clone();
        let get_fn: CtrlGetFn = Box::new(move || {
            let mut buf = vec![0u8; def.size];
            crate::UvcDevice::get_pu_control(
                h.as_ref(),
                vc_iface,
                unit_id,
                def.selector,
                RequestCode::GetCur,
                &mut buf,
            )
            .map_err(|_e| v4l2_core::V4l2Error::Io)?;
            let raw = decode_uvc_value(&buf).ok_or(v4l2_core::V4l2Error::Io)?;
            // ExposureAuto：UVC 位掩码（1<<n）→ V4L2 菜单值（n+1）
            if def.cid == CameraClassCtrl::ExposureAuto as u32 {
                Ok(raw.trailing_zeros() as i64 + 1)
            } else {
                Ok(raw)
            }
        });

        let h = handle.clone();
        let set_fn: CtrlSetFn = Box::new(move |v| {
            // ExposureAuto：V4L2 菜单值（n+1）→ UVC 位掩码（1<<n）
            let v = if def.cid == CameraClassCtrl::ExposureAuto as u32 {
                1i64 << (v - 1)
            } else {
                v
            };
            let buf = encode_uvc_value(v, def.size).ok_or(v4l2_core::V4l2Error::Io)?;
            crate::UvcDevice::send_pu_control(h.as_ref(), vc_iface, unit_id, def.selector, &buf)
                .map_err(|_| v4l2_core::V4l2Error::Io)?;
            Ok(v)
        });

        // 4. 注册：硬件代理控件（Linux v4l2_ctrl_ops 形态）。VOLATILE 表示
        //    G 读取时走 get 回调（从设备取当前值），S 时始终写设备。
        let ops = CtrlOps {
            get: Some(get_fn),
            try_ctrl: None,
            set: set_fn,
        };
        let cfg = if def.menu_items > 0 {
            let qmenu = if def.cid == UserClassCtrl::PowerLineFrequency as u32 {
                POWER_LINE_FREQ_MENU
            } else {
                EXPOSURE_AUTO_MENU
            };
            CtrlConfig {
                id: def.cid,
                name: def.name,
                ctrl_type: CtrlType::Menu,
                minimum: 0,
                maximum: def.menu_items as i64 - 1,
                step: 0,
                default_value: default.clamp(0, def.menu_items as i64 - 1),
                flags: CtrlFlags::VOLATILE,
                qmenu: Some(qmenu),
                ops: Some(ops),
            }
        } else if def.is_bool {
            CtrlConfig {
                id: def.cid,
                name: def.name,
                ctrl_type: CtrlType::Boolean,
                minimum: 0,
                maximum: 1,
                step: 1,
                default_value: default.clamp(0, 1),
                flags: CtrlFlags::VOLATILE,
                qmenu: None,
                ops: Some(ops),
            }
        } else {
            CtrlConfig {
                id: def.cid,
                name: def.name,
                ctrl_type: CtrlType::Integer,
                minimum: min,
                maximum: max,
                step: step as u64,
                default_value: default,
                flags: CtrlFlags::VOLATILE,
                qmenu: None,
                ops: Some(ops),
            }
        };
        ctrls
            .new_ctrl(cfg)
            .expect("uvc control registration must succeed");
    }
}
