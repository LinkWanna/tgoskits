//! UVC V4L2 camera driver — kernel-side glue.

use alloc::sync::Arc;
use core::task::{Context, Poll};

use crab_usb::{
    IsoIrqCallback,
    err::USBError,
    usb_if::{
        endpoint::{TransferCompletion, TransferRequest},
        host::ControlSetup,
        transfer::{Recipient, RequestType},
    },
};
use media_uvc::{
    UvcDevice, UvcHandle,
    stream::{IsoBatchResult, IsoStreamHandle},
};

use crate::{
    StarryError,
    pseudofs::usbfs::{self, SubmittedTransfer, UsbDeviceHandle, UsbDeviceSnapshotInfo},
};

/// 将 usbfs 层错误映射为 [`USBError`]（media-uvc 的错误类型）。
fn map_usb_error(e: StarryError) -> USBError {
    use StarryError::*;
    match e {
        InvalidInput => USBError::InvalidParameter,
        NotFound | NoSuchDevice | NoSuchDeviceOrAddress => USBError::NotFound,
        ResourceBusy => USBError::SlotLimitReached,
        Unsupported | NotATty => USBError::NotSupported,
        TimedOut => USBError::Timeout,
        NoMemory => USBError::NoMemory,
        // 取消的传输：usbfs 层按 Linux 语义映射为 ENOENT
        // （map_transfer_error），这里还原为 Cancelled——media-uvc 流
        // worker 依赖它区分正常 STREAMOFF 取消与真实错误。
        Errno(crate::Errno::ENOENT) => {
            USBError::TransferError(crab_usb::usb_if::err::TransferError::Cancelled)
        }
        OperationNotPermitted | PermissionDenied => {
            USBError::Other(anyhow::anyhow!("usbfs: operation not permitted: {e}"))
        }
        other => USBError::Other(anyhow::anyhow!("usbfs: {other}")),
    }
}

// ── UvcHandle impl for UsbDeviceHandle ────────────────────────────────

impl UvcHandle for UsbDeviceHandle {
    fn claim_interface(&self, interface: u8, alternate: u8) -> Result<(), USBError> {
        UsbDeviceHandle::claim_interface(self, interface, alternate).map_err(map_usb_error)
    }

    fn release_interface(&self, interface: u8) -> Result<(), USBError> {
        UsbDeviceHandle::release_interface(self, interface).map_err(map_usb_error)
    }

    fn control_in(&self, param: ControlSetup, data: &mut [u8]) -> Result<usize, USBError> {
        let bmrt = control_setup_to_bmrequesttype(&param) | 0x80; // IN
        let req = control_setup_to_brequest(&param);
        self.control_transfer(bmrt, req, param.value, param.index, data)
            .map_err(map_usb_error)
    }

    fn control_out(&self, param: ControlSetup, data: &[u8]) -> Result<(), USBError> {
        let bmrt = control_setup_to_bmrequesttype(&param) & !0x80; // OUT
        let req = control_setup_to_brequest(&param);
        let mut buf = [0u8; 64];
        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);
        let _ = self
            .control_transfer(bmrt, req, param.value, param.index, &mut buf[..len])
            .map_err(map_usb_error)?;
        Ok(())
    }

    fn submit_iso_batch(
        &self,
        endpoint: u8,
        data: &mut [u8],
        packet_lengths: &[usize],
    ) -> Result<Arc<dyn IsoStreamHandle>, USBError> {
        let transfer = self
            .submit_endpoint_transfer(endpoint, TransferRequest::iso_in(data, packet_lengths))
            .map_err(map_usb_error)?;
        Ok(Arc::new(KernelIsoBatch { transfer }))
    }

    fn set_iso_irq_callback(
        &self,
        endpoint: u8,
        cb: Option<IsoIrqCallback>,
    ) -> Result<(), USBError> {
        self.lease_set_iso_irq_callback(endpoint, cb)
            .map_err(map_usb_error)
    }

    fn halt_iso_stream(&self, endpoint: u8) -> Result<(), USBError> {
        self.lease_halt_iso_stream(endpoint).map_err(map_usb_error)
    }
}

/// [`SubmittedTransfer`] 的 [`IsoStreamHandle`] 适配器（media-uvc 流 worker
/// 轮询 + 取消的 trait 对象）。等长槽布局：槽 i 数据 =
/// `batch_buf[i * slot_len .. i * slot_len + actuals[i]]`。
struct KernelIsoBatch {
    transfer: SubmittedTransfer,
}

impl IsoStreamHandle for KernelIsoBatch {
    fn poll(&self, cx: &mut Context<'_>) -> Poll<Result<IsoBatchResult, USBError>> {
        match self.transfer.poll_reclaim(cx) {
            Poll::Ready(result) => Poll::Ready(
                result
                    .map(|completion: TransferCompletion| IsoBatchResult {
                        total: completion.actual_length,
                        actuals: completion
                            .iso_packets
                            .iter()
                            .map(|packet| packet.actual_length)
                            .collect(),
                    })
                    .map_err(map_usb_error),
            ),
            Poll::Pending => Poll::Pending,
        }
    }

    fn cancel(&self) -> Result<(), USBError> {
        self.transfer.cancel().map_err(map_usb_error)
    }
}

fn control_setup_to_bmrequesttype(setup: &ControlSetup) -> u8 {
    use Recipient::*;
    use RequestType::*;
    let ty_bits = match setup.request_type {
        Standard => 0x00,
        Class => 0x20,
        Vendor => 0x40,
        Reserved => 0x60,
    };
    let recip_bits = match setup.recipient {
        Device => 0x00,
        Interface => 0x01,
        Endpoint => 0x02,
        Other => 0x03,
    };
    ty_bits | recip_bits
}

fn control_setup_to_brequest(setup: &ControlSetup) -> u8 {
    // Request is #[repr(u8)] with IntoPrimitive
    setup.request.into()
}

// ── Camera driver creation ───────────────────────────────────────────

pub type CameraDriver = UvcDevice<UsbDeviceHandle>;

pub fn create_camera_driver(snap: &UsbDeviceSnapshotInfo) -> CameraDriver {
    let handle =
        usbfs::acquire_usb_device(snap.bus_num, snap.device_num).expect("acquire USB failed");

    // VC/VS 接口号从快照描述符 blob 解析，不再硬编码；blob 仅用于初始化。
    UvcDevice::new(handle, &snap.descriptor_blob).expect("parse UVC interface failed")
}
