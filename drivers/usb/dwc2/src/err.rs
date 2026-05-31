//! DWC2 驱动内部错误类型。

/// DWC2 驱动操作结果。
pub type Result<T> = core::result::Result<T, Error>;

/// DWC2 驱动错误。
#[derive(Debug, Clone)]
pub enum Error {
    /// 硬件超时
    Timeout,
    /// 端点 STALL
    Stall,
    /// 设备无响应（NAK 耗尽）
    NakExhausted,
    /// 传输错误（CRC/PID/babble）
    Transfer,
    /// 通道忙
    ChannelBusy,
    /// 无可用通道
    NoChannel,
    /// 无设备连接
    NoDevice,
    /// 参数无效
    InvalidParam,
    /// DMA 缓冲区不足
    DmaTooSmall,
    /// 不支持的传输类型
    NotSupported,
    /// 其他错误
    Other(&'static str),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Timeout => write!(f, "DWC2 timeout"),
            Error::Stall => write!(f, "DWC2 STALL"),
            Error::NakExhausted => write!(f, "DWC2 NAK exhausted"),
            Error::Transfer => write!(f, "DWC2 transfer error"),
            Error::ChannelBusy => write!(f, "DWC2 channel busy"),
            Error::NoChannel => write!(f, "DWC2 no free channel"),
            Error::NoDevice => write!(f, "DWC2 no device connected"),
            Error::InvalidParam => write!(f, "DWC2 invalid parameter"),
            Error::DmaTooSmall => write!(f, "DWC2 DMA buffer too small"),
            Error::NotSupported => write!(f, "DWC2 operation not supported"),
            Error::Other(msg) => write!(f, "DWC2 error: {msg}"),
        }
    }
}
