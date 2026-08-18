use tock_registers::interfaces::{Readable, Writeable};

use crate::backend::{
    kmod::dwc2::{
        channel::Dwc2ChannelCompletions,
        reg::{
            DWC2_MAX_CHANNELS, DWC2_RUNTIME_GINTMSK, Dwc2Registers, GINTSTS_DISCONNINT,
            GINTSTS_HCHINT, GINTSTS_PRTINT, HCINT_CHHLTD, HPRT_CONN_DET, HPRT_ENA_CHG,
            HPRT_OVRCUR_CHG, HPRT_W1C_MASK,
        },
        stats::Dwc2Stats,
    },
    ty::{Event, EventHandlerOp},
};

pub(crate) struct Dwc2EventHandler {
    regs: Dwc2Registers,
    channel_completions: Dwc2ChannelCompletions,
    stats: Dwc2Stats,
}

unsafe impl Send for Dwc2EventHandler {}
unsafe impl Sync for Dwc2EventHandler {}

impl Dwc2EventHandler {
    pub(crate) fn new(
        regs: Dwc2Registers,
        channel_completions: Dwc2ChannelCompletions,
        stats: Dwc2Stats,
    ) -> Self {
        Self {
            regs,
            channel_completions,
            stats,
        }
    }
}

impl EventHandlerOp for Dwc2EventHandler {
    fn handle_event(&self) -> Event {
        let pending =
            self.regs.regs().gintsts.get() & self.regs.regs().gintmsk.get() & DWC2_RUNTIME_GINTMSK;
        if pending == 0 {
            return Event::Nothing;
        }

        if pending & GINTSTS_DISCONNINT != 0 {
            self.channel_completions.disconnect_all_with(|| {
                self.regs.regs().haintmsk.set(0);
                let mask = self.regs.regs().gintmsk.get();
                self.regs.regs().gintmsk.set(mask & !GINTSTS_HCHINT);
            });
            self.regs.regs().gintsts.set(GINTSTS_DISCONNINT);
            return Event::Stopped;
        }

        if pending & GINTSTS_PRTINT != 0 {
            let hprt = self.regs.hprt().raw();
            let changes = hprt & (HPRT_CONN_DET | HPRT_ENA_CHG | HPRT_OVRCUR_CHG);
            if changes != 0 {
                // PRTINT is a read-only summary. Linux clears its source by
                // acknowledging the HPRT0 W1C change bits while writing zero
                // to HPRT0.ENA so the acknowledgement cannot disable the port.
                self.regs.hprt().write((hprt & !HPRT_W1C_MASK) | changes);
            }
            return Event::PortChange { port: 1 };
        }
        if pending & GINTSTS_HCHINT != 0 {
            self.stats.record_irq_event();
            let count = self.handle_channel_interrupts();
            self.regs.regs().gintsts.set(GINTSTS_HCHINT);
            return Event::TransferActivity {
                count: count.max(1),
            };
        }
        self.regs.regs().gintsts.set(pending);
        Event::Stopped
    }
}

impl Dwc2EventHandler {
    fn handle_channel_interrupts(&self) -> usize {
        let pending = self.regs.regs().haint.get() & self.regs.regs().haintmsk.get();
        let mut count = 0usize;
        for channel in 0..DWC2_MAX_CHANNELS {
            if pending & (1u32 << channel) == 0 {
                continue;
            }
            let channel_regs = self.regs.channel(channel);
            let Some(hcint) = channel_regs.take_irqs() else {
                continue;
            };
            if hcint & HCINT_CHHLTD == 0 && channel_regs.is_enabled() {
                if self.channel_completions.is_iso(channel) {
                    // ISO 常驻通道：XFERCOMPL（IOC）时保持通道使能并继续周期
                    // 会话，直接发布完成位，由任务侧结算本请求。
                    self.channel_completions.publish(channel, hcint);
                    self.stats.record_channel_completion();
                    count += 1;
                } else {
                    self.channel_completions.defer(channel, hcint);
                    channel_regs.disable();
                }
                continue;
            }
            self.channel_completions.publish(channel, hcint);
            self.stats.record_channel_completion();
            count += 1;
        }
        count
    }
}
