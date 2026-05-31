//! USB 设备速度类型。
//!
//! Re-export 自 usb-if 以保持统一的类型定义。

pub use usb_if::host::hub::Speed;

/// 从 HPRT0[18:17] 速度位解析。
///
/// DWC2 HPRT0.SPD 编码：0=HS, 1=FS, 2=LS。
#[inline]
pub fn speed_from_hprt_bits(bits: u32) -> Speed {
    match bits & 3 {
        0 => Speed::High,
        1 => Speed::Full,
        2 => Speed::Low,
        _ => Speed::Full,
    }
}
