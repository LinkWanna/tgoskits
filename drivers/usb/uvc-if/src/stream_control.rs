//! UVC 1.0/1.1 26-byte VS PROBE and COMMIT wire format.

use usb_if::err::USBError;

pub const STREAM_CONTROL_LEN: usize = 26;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamControl {
    pub hint: u16,
    pub format_index: u8,
    pub frame_index: u8,
    pub frame_interval: u32,
    pub key_frame_rate: u16,
    pub p_frame_rate: u16,
    pub comp_quality: u16,
    pub comp_window_size: u16,
    pub delay: u16,
    pub max_video_frame_size: u32,
    pub max_payload_transfer_size: u32,
}

impl StreamControl {
    pub fn to_bytes(&self) -> [u8; STREAM_CONTROL_LEN] {
        let mut data = [0; STREAM_CONTROL_LEN];
        data[0..2].copy_from_slice(&self.hint.to_le_bytes());
        data[2] = self.format_index;
        data[3] = self.frame_index;
        data[4..8].copy_from_slice(&self.frame_interval.to_le_bytes());
        data[8..10].copy_from_slice(&self.key_frame_rate.to_le_bytes());
        data[10..12].copy_from_slice(&self.p_frame_rate.to_le_bytes());
        data[12..14].copy_from_slice(&self.comp_quality.to_le_bytes());
        data[14..16].copy_from_slice(&self.comp_window_size.to_le_bytes());
        data[16..18].copy_from_slice(&self.delay.to_le_bytes());
        data[18..22].copy_from_slice(&self.max_video_frame_size.to_le_bytes());
        data[22..26].copy_from_slice(&self.max_payload_transfer_size.to_le_bytes());
        data
    }

    pub fn parse(data: &[u8]) -> Result<Self, USBError> {
        let data = data
            .get(..STREAM_CONTROL_LEN)
            .ok_or(USBError::InvalidParameter)?;
        Ok(Self {
            hint: u16::from_le_bytes(data[0..2].try_into().unwrap()),
            format_index: data[2],
            frame_index: data[3],
            frame_interval: u32::from_le_bytes(data[4..8].try_into().unwrap()),
            key_frame_rate: u16::from_le_bytes(data[8..10].try_into().unwrap()),
            p_frame_rate: u16::from_le_bytes(data[10..12].try_into().unwrap()),
            comp_quality: u16::from_le_bytes(data[12..14].try_into().unwrap()),
            comp_window_size: u16::from_le_bytes(data[14..16].try_into().unwrap()),
            delay: u16::from_le_bytes(data[16..18].try_into().unwrap()),
            max_video_frame_size: u32::from_le_bytes(data[18..22].try_into().unwrap()),
            max_payload_transfer_size: u32::from_le_bytes(data[22..26].try_into().unwrap()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_control_round_trip_and_short_read() {
        let control = StreamControl {
            hint: 0x1234,
            format_index: 2,
            frame_index: 3,
            frame_interval: 333_333,
            key_frame_rate: 4,
            p_frame_rate: 5,
            comp_quality: 6,
            comp_window_size: 7,
            delay: 8,
            max_video_frame_size: 614_400,
            max_payload_transfer_size: 3_072,
        };
        let bytes = control.to_bytes();
        assert_eq!(StreamControl::parse(&bytes).unwrap(), control);
        assert!(StreamControl::parse(&bytes[..25]).is_err());
    }
}
