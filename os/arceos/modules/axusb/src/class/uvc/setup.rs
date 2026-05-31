//! UVC SETUP packet 构造 + 低层控制传输 helper。

use ax_driver::prelude::SetupPacket;

use crate::Device;

// ── SETUP packet ──

pub(crate) fn setup_get_descriptor_config(cfg_index: u8, w_length: u16) -> SetupPacket {
    SetupPacket::new(0x80, 0x06, (2u16 << 8) | cfg_index as u16, 0, w_length)
}

pub(crate) fn uvc_set_cur_vs(interface: u8, selector: u8, w_length: u16) -> SetupPacket {
    SetupPacket::new(
        0x21,
        0x01,
        (selector as u16) << 8,
        interface as u16,
        w_length,
    )
}

pub(crate) fn uvc_get_cur_vs(interface: u8, selector: u8, w_length: u16) -> SetupPacket {
    SetupPacket::new(
        0xA1,
        0x81,
        (selector as u16) << 8,
        interface as u16,
        w_length,
    )
}

pub(crate) fn uvc_get_max_vs(interface: u8, selector: u8, w_length: u16) -> SetupPacket {
    SetupPacket::new(
        0xA1,
        0x83,
        (selector as u16) << 8,
        interface as u16,
        w_length,
    )
}

pub(crate) fn uvc_set_cur_vc(
    interface: u8,
    entity_id: u8,
    selector: u8,
    w_length: u16,
) -> SetupPacket {
    SetupPacket::new(
        0x21,
        0x01,
        (selector as u16) << 8,
        (entity_id as u16) << 8 | interface as u16,
        w_length,
    )
}

pub(crate) fn uvc_get_cur_vc(
    interface: u8,
    entity_id: u8,
    selector: u8,
    w_length: u16,
) -> SetupPacket {
    SetupPacket::new(
        0xA1,
        0x81,
        (selector as u16) << 8,
        (entity_id as u16) << 8 | interface as u16,
        w_length,
    )
}

pub(crate) fn uvc_get_def_vc(
    interface: u8,
    entity_id: u8,
    selector: u8,
    w_length: u16,
) -> SetupPacket {
    SetupPacket::new(
        0xA1,
        0x87,
        (selector as u16) << 8,
        (entity_id as u16) << 8 | interface as u16,
        w_length,
    )
}

// ── 控制传输 helper ──

pub(crate) fn set_cur_u8(
    device: &mut Device,
    vc_if: u8,
    entity: u8,
    selector: u8,
    value: u8,
) -> bool {
    device
        .control_out(uvc_set_cur_vc(vc_if, entity, selector, 1), &[value])
        .is_ok()
}

pub(crate) fn get_cur_u16(device: &mut Device, vc_if: u8, entity: u8, selector: u8) -> Option<u16> {
    let mut buf = [0u8; 2];
    device
        .control_in(uvc_get_cur_vc(vc_if, entity, selector, 2), &mut buf)
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
    device
        .control_out(
            uvc_set_cur_vc(vc_if, entity, selector, 2),
            &value.to_le_bytes(),
        )
        .is_ok()
}

pub(crate) fn get_def_u16(device: &mut Device, vc_if: u8, entity: u8, selector: u8) -> Option<u16> {
    let mut buf = [0u8; 2];
    device
        .control_in(uvc_get_def_vc(vc_if, entity, selector, 2), &mut buf)
        .is_ok()
        .then(|| u16::from_le_bytes(buf))
}
