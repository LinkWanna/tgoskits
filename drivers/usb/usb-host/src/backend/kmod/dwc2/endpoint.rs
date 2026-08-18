use alloc::boxed::Box;
use core::task::Context;

use usb_if::{
    descriptor::EndpointType,
    endpoint::{EndpointInfo, RequestId, TransferCompletion, TransferRequest},
    err::TransferError,
    host::hub::Speed,
};

use crate::{
    backend::kmod::{
        Kernel,
        dwc2::{
            channel::{
                ChannelConfig, HostChannelPool, iso::IsoChannelState, non_iso::NonIsoChannelState,
            },
            endpoint_type_to_dwc2,
            reg::Dwc2Registers,
            stats::Dwc2Stats,
        },
    },
    err::Result,
};

pub(crate) struct Dwc2Endpoint {
    config: ChannelConfig,
    non_iso: NonIsoChannelState,
    iso: IsoChannelState,
}

unsafe impl Send for Dwc2Endpoint {}

pub(crate) struct Dwc2EndpointParams {
    pub(crate) regs: Dwc2Registers,
    pub(crate) kernel: Kernel,
    pub(crate) device_address: u8,
    pub(crate) port_speed: Speed,
    pub(crate) info: EndpointInfo,
    pub(crate) channel_pool: HostChannelPool,
    pub(crate) stats: Dwc2Stats,
}

impl Dwc2Endpoint {
    pub(crate) fn new(params: Dwc2EndpointParams) -> Result<Self> {
        let Dwc2EndpointParams {
            regs,
            kernel,
            device_address,
            port_speed,
            info,
            channel_pool,
            stats,
        } = params;
        endpoint_type_to_dwc2(info.transfer_type)?;
        let config = ChannelConfig {
            device_address,
            info,
            port_speed,
        };
        Ok(Self {
            config,
            non_iso: NonIsoChannelState::new(
                regs,
                kernel.clone(),
                stats.clone(),
                channel_pool.clone(),
            ),
            iso: IsoChannelState::new(regs, kernel, stats, channel_pool),
        })
    }

    pub(crate) fn set_device_address(&mut self, address: u8) {
        self.config.device_address = address;
    }

    pub(crate) fn set_max_packet_size(&mut self, max_packet_size: u8) {
        self.config.info.max_packet_size = u16::from(max_packet_size).max(8);
    }

    /// 在飞或已完成的请求 id（`Dwc2Device::quiesce_endpoints` 停稳前查询）。
    pub(crate) fn in_flight_request_id(&self) -> Option<RequestId> {
        match self.config.info.transfer_type {
            EndpointType::Isochronous => self.iso.in_flight_request_id(),
            _ => self.non_iso.in_flight_request_id(),
        }
    }
}

impl crate::backend::ty::ep::EndpointOp for Dwc2Endpoint {
    fn submit_request(
        &mut self,
        request: TransferRequest,
    ) -> core::result::Result<RequestId, TransferError> {
        // 通道由各状态机内部从池中租借（ISO 常驻会话，non-ISO 按请求）。
        if matches!(request, TransferRequest::Isochronous { .. }) {
            return self.iso.submit(&self.config, request);
        }
        self.non_iso.submit(&self.config, request)
    }

    fn reclaim_request(
        &mut self,
        id: RequestId,
    ) -> Option<core::result::Result<TransferCompletion, TransferError>> {
        match self.config.info.transfer_type {
            EndpointType::Isochronous => self.iso.reclaim(id),
            _ => self.non_iso.reclaim(id),
        }
    }

    fn register_waker(&self, id: RequestId, cx: &mut Context<'_>) {
        match self.config.info.transfer_type {
            EndpointType::Isochronous => self.iso.register_waker(id, cx),
            _ => self.non_iso.register_waker(id, cx),
        }
    }

    fn cancel_request(&mut self, id: RequestId) -> core::result::Result<(), TransferError> {
        match self.config.info.transfer_type {
            EndpointType::Isochronous => self.iso.cancel(id),
            _ => self.non_iso.cancel(id),
        }
    }

    fn reset(&mut self) -> crate::backend::ty::ep::EndpointResetFuture {
        let result = match self.config.info.transfer_type {
            EndpointType::Isochronous => self.iso.reset(),
            _ => self.non_iso.reset(),
        };
        Box::pin(async move { result })
    }
}
