use thiserror::Error;

/// V4L2 操作结果。
pub type Result<T> = core::result::Result<T, V4l2Error>;

/// 映射到 Linux errno 的 V4L2 错误码。
///
/// 错误码到 StarryOS `AxError` 的转换只发生在 glue 层
/// （`pseudofs/dev/video.rs::v4l2_to_axerror`），此处不维护第二套映射。
#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum V4l2Error {
    /// -EINVAL 非法参数
    #[error("invalid argument")]
    InvalidArgument,
    /// -ENODEV 设备不存在
    #[error("no such device")]
    NoSuchDevice,
    /// -EIO I/O 错误
    #[error("I/O error")]
    Io,
    /// -ENOTSUP 操作不支持
    #[error("operation not supported")]
    NotSupported,
    /// -EBUSY 设备或资源忙
    #[error("device or resource busy")]
    Busy,
    /// -ETIMEDOUT 超时
    #[error("timed out")]
    Timeout,
    /// -ENOMEM 内存不足
    #[error("out of memory")]
    NoMemory,
    /// -EACCES 拒绝访问
    #[error("access denied")]
    AccessDenied,
    /// -EBADF 坏文件描述符
    #[error("bad file descriptor")]
    BadFileDescriptor,
    /// -EAGAIN 资源暂时不可用，请重试（非阻塞 DQBUF 无缓冲等）
    #[error("try again")]
    WouldBlock,
    /// -ENOENT 不存在
    #[error("no such file or entry")]
    NoEntry,
    /// -ENXIO 无此设备或地址
    #[error("no such device or address")]
    NoSuchDeviceOrAddress,
    /// -EPERM 操作不被允许
    #[error("operation not permitted")]
    OperationNotPermitted,
    /// -EINTR 操作被信号中断
    #[error("interrupted")]
    Interrupted,
    /// -ENOTTY ioctl 不适用于此设备
    #[error("inappropriate ioctl for device")]
    NotATty,
    /// -ENOSPC 空间不足
    #[error("no space left on device")]
    StorageFull,
    /// -ERANGE 数值越界
    #[error("result out of range")]
    OutOfRange,
    /// -EMSGSIZE 消息过长
    #[error("message too long")]
    MessageTooLong,
}
