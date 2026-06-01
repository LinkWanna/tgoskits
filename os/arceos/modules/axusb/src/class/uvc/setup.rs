//! UVC SETUP packet 构造 + 低层控制传输 helper。

use ax_driver::prelude::SetupPacket;

use crate::Device;

// ── UVC 请求枚举 ──

/// UVC 控制操作类型。
#[derive(Clone, Copy)]
pub(crate) enum UvcOp {
    SetCur,
    GetCur,
    GetMax,
    GetDef,
}

impl UvcOp {
    /// 返回 (bmRequestType, bRequest)。
    fn raw(self) -> (u8, u8) {
        match self {
            UvcOp::SetCur => (0x21, 0x01),
            UvcOp::GetCur => (0xA1, 0x81),
            UvcOp::GetMax => (0xA1, 0x83),
            UvcOp::GetDef => (0xA1, 0x87),
        }
    }
}

/// UVC 类控制请求。
///
/// 两种变体对应 USB spec 中 wIndex 编码不同
/// 的两个 UVC 接口子类（VideoControl / VideoStreaming）。
pub(crate) enum UvcRequest {
    /// VideoStreaming 接口（wIndex = interface）。
    Streaming {
        interface: u8,
        selector: u8,
        op: UvcOp,
        len: u16,
    },
    /// VideoControl 接口（wIndex = (entity_id << 8) | interface）。
    Control {
        interface: u8,
        entity_id: u8,
        selector: u8,
        op: UvcOp,
        len: u16,
    },
}

impl UvcRequest {
    /// 构造 VideoStreaming 接口的 PROBE/COMMIT 请求。
    pub(crate) fn streaming(interface: u8, selector: u8, op: UvcOp, len: u16) -> Self {
        Self::Streaming {
            interface,
            selector,
            op,
            len,
        }
    }

    /// 构造 VideoControl 接口的相机控制请求。
    pub(crate) fn control(interface: u8, entity_id: u8, selector: u8, op: UvcOp, len: u16) -> Self {
        Self::Control {
            interface,
            entity_id,
            selector,
            op,
            len,
        }
    }

    /// 转换为 `SetupPacket`。
    pub(crate) fn to_setup(&self) -> SetupPacket {
        match *self {
            Self::Streaming {
                interface,
                selector,
                op,
                len,
            } => {
                let (bm, br) = op.raw();
                SetupPacket::new(bm, br, (selector as u16) << 8, interface as u16, len)
            }
            Self::Control {
                interface,
                entity_id,
                selector,
                op,
                len,
            } => {
                let (bm, br) = op.raw();
                let w_index = (entity_id as u16) << 8 | interface as u16;
                SetupPacket::new(bm, br, (selector as u16) << 8, w_index, len)
            }
        }
    }
}

// ── 标准请求 ──

pub(crate) fn setup_get_descriptor_config(cfg_index: u8, w_length: u16) -> SetupPacket {
    SetupPacket::new(0x80, 0x06, (2u16 << 8) | cfg_index as u16, 0, w_length)
}

// ── 控制传输 helper ──

pub(crate) fn set_cur_u8(
    device: &mut Device,
    vc_if: u8,
    entity: u8,
    selector: u8,
    value: u8,
) -> bool {
    let setup = UvcRequest::control(vc_if, entity, selector, UvcOp::SetCur, 1).to_setup();
    device.control_out(setup, &[value]).is_ok()
}

pub(crate) fn get_cur_u16(device: &mut Device, vc_if: u8, entity: u8, selector: u8) -> Option<u16> {
    let mut buf = [0u8; 2];
    let setup = UvcRequest::control(vc_if, entity, selector, UvcOp::GetCur, 2).to_setup();
    device
        .control_in(setup, &mut buf)
        .is_ok()
        .then(|| u16::from_le_bytes(buf))
}

pub(crate) fn set_cur_u16(
    device: &mut Device,
    vc_if: u8,
    entity: u8,
    selector: u8,
    value: u16,
) -> bool {
    let setup = UvcRequest::control(vc_if, entity, selector, UvcOp::SetCur, 2).to_setup();
    device.control_out(setup, &value.to_le_bytes()).is_ok()
}

pub(crate) fn get_def_u16(device: &mut Device, vc_if: u8, entity: u8, selector: u8) -> Option<u16> {
    let mut buf = [0u8; 2];
    let setup = UvcRequest::control(vc_if, entity, selector, UvcOp::GetDef, 2).to_setup();
    device
        .control_in(setup, &mut buf)
        .is_ok()
        .then(|| u16::from_le_bytes(buf))
}
