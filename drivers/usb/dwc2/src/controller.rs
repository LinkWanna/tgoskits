//! DWC2 控制器 — 硬件初始化、复位、枚举辅助。
//!
//! 管理通道池（[`HostChannel`; 16]）和根端口操作。

use alloc::boxed::Box;

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use crate::{
    channel::{self, EpType, HostChannel},
    dma::DmaBuffer,
    err::Error,
    mmio::Dwc2Mmio,
    osal::Osal,
    reg::{
        GAHBCFG, GDFIFOCFG, GHWCFG2, GHWCFG3, GHWCFG4, GINTMSK, GINTSTS, GOTGCTL, GRSTCTL, GUSBCFG,
        HCFG,
    },
    speed::{self, Speed},
};

// ── Core revision constants ──
const DWC2_CORE_REV_2_91A: u32 = 0x4f54_291a;
const DWC2_CORE_REV_4_20A: u32 = 0x4f54_420a;
const DWC2_CORE_REV_MASK: u32 = 0xffff;

// ── HPRT0 W1C mask (Linux dwc2_clear_hprt_intr_bits) ──
const HPRT0_W1C_MASK: u32 = (1 << 1) | (1 << 2) | (1 << 3) | (1 << 5);

/// DWC2 主机控制器。
pub struct Dwc2Controller {
    /// MMIO 视图
    mmio: &'static Dwc2Mmio,
    /// OS 抽象
    osal: &'static dyn Osal,
    /// 通道池
    channels: [Option<HostChannel>; 16],
    /// 全局 DMA 缓冲区（整个控制器生命周期共享）
    dma_buf: Option<DmaBuffer>,
    /// 设备速度（枚举后确定）
    speed: Option<Speed>,
}

impl Dwc2Controller {
    /// 构造控制器（尚未初始化硬件）。
    ///
    /// # Safety
    ///
    /// `base` 必须为有效的 DWC2 MMIO 虚拟基址。
    /// `osal` 必须提供正确的 DMA/延时/V2P 实现。
    pub unsafe fn new(base: usize, osal: &'static dyn Osal) -> Self {
        unsafe {
            let mmio = Box::leak(Box::new(Dwc2Mmio::new(base)));
            Self {
                mmio,
                osal,
                channels: Default::default(),
                dma_buf: None,
                speed: None,
            }
        }
    }

    /// 获取 MMIO 视图。
    #[inline]
    pub fn mmio(&self) -> &'static Dwc2Mmio {
        self.mmio
    }

    /// 获取设备速度。
    #[inline]
    pub fn speed(&self) -> Option<Speed> {
        self.speed
    }

    /// 硬件初始化（SG2002/CV182x 路径）。
    ///
    /// 包含：软复位、强制 Host 模式、FIFO 配置、DMA 使能、根口上电。
    /// 调用后应调用 `wait_connect()` 等待设备连接。
    pub fn hw_init(&mut self) -> Result<(), Error> {
        let r = self.mmio.regs();

        // 禁用全部中断（轮询模式）
        r.gintmsk.set(0);
        r.gintsts.set(0xFFFF_FFFF);

        // 软复位
        self.core_soft_reset()?;
        self.force_host_mode()?;
        self.core_soft_reset()?;

        // OTG session override（确保 VBUS 有效）
        self.init_gotgctl_override();

        // GUSBCFG: Force Host + UTMI 16-bit 自适配
        self.init_gusbcfg();

        // PCGCTL 清零
        r.pcgctl.set(0);

        // GAHBCFG: DMA + burst
        self.init_gahb();

        // HCFG: 仅 HS
        self.init_hcfg();

        // FIFO 配置
        self.init_fifos()?;

        // Flush FIFO
        self.flush_tx_fifo_all()?;
        self.flush_rx_fifo()?;

        // 中断掩码（仅通道中断用于等待 CHHLTD）
        r.haintmsk.set((1 << 0) | (1 << 1));
        r.gintmsk.modify(GINTMSK::HCHINT::SET);
        r.gintsts.set(0xFFFF_FFFF);

        // 根口上电
        self.port_power_on();

        // CV182x PHY UTMI override 清零
        self.clear_utmi_override();

        // 再次 OTG session override（上电后）
        self.init_gotgctl_override();

        log::info!("DWC2: hardware initialized");
        // 打印关键寄存器用于调试
        let r = self.mmio.regs();
        log::info!(
            "DWC2 regs: GAHBCFG={:#010x} GUSBCFG={:#010x} GINTMSK={:#010x} HCFG={:#010x} \
             HPRT0={:#010x}",
            r.gahbcfg.get(),
            r.gusbcfg.get(),
            r.gintmsk.get(),
            r.hcfg.get(),
            r.hprt0.get()
        );
        Ok(())
    }

    /// 分配 DMA 缓冲区（整个控制器共享）。
    pub fn alloc_dma_buf(&mut self, size: usize) -> Result<(), Error> {
        let (va, pa, actual_size) = self.osal.dma_alloc(size).ok_or(Error::DmaTooSmall)?;
        self.dma_buf = Some(unsafe { DmaBuffer::from_raw(va, pa, actual_size) });
        Ok(())
    }

    /// 获取 DMA 缓冲区引用。
    pub fn dma_buf(&self) -> Option<&DmaBuffer> {
        self.dma_buf.as_ref()
    }

    /// 清理（写回）DMA 缓冲区 cache 到内存。
    /// 在 DMA 从该地址**读取**之前调用（OUT 传输）。
    pub fn dma_cache_clean(&self, va: *const u8, len: usize) {
        self.osal.dma_cache_clean(va, len);
    }

    /// 失效（丢弃）DMA 缓冲区 cache。
    /// 在 DMA 向该地址**写入**之后调用（IN 传输）。
    pub fn dma_cache_invalidate(&self, va: *const u8, len: usize) {
        self.osal.dma_cache_invalidate(va, len);
    }

    /// 等待设备连接，返回检测到的速度。
    pub fn wait_connect(&self) -> Result<Speed, Error> {
        let r = self.mmio.regs();
        for _ in 0..10_000_000u32 {
            let hprt = r.hprt0.get();
            if hprt & 1 != 0 {
                // CONNSTS
                let speed = speed::speed_from_hprt_bits(hprt >> 17);
                log::info!("DWC2: device connected, speed={speed:?}");
                return Ok(speed);
            }
            self.spin_delay(100);
        }
        Err(Error::NoDevice)
    }

    /// 根端口复位（USB 总线复位）。
    ///
    /// 发出 ≥50ms 的 SE0，复位后等待设备恢复。
    pub fn root_port_reset(&mut self, _speed: Speed) -> Result<(), Error> {
        let r = self.mmio.regs();

        // 清除 CONNDET
        let cur = r.hprt0.get();
        if cur & (1 << 1) != 0 {
            r.hprt0.set((cur & !HPRT0_W1C_MASK) | (1 << 1));
        }

        // 保持 PWR，拉 PRTRST
        let base = (r.hprt0.get() & !HPRT0_W1C_MASK) | (1 << 12);
        r.hprt0.set(base | (1 << 8));

        // PRTRST ≥ 60ms
        self.spin_delay(15_000_000);

        // 解 PRTRST
        let base2 = (r.hprt0.get() & !HPRT0_W1C_MASK) | (1 << 12);
        r.hprt0.set(base2 & !(1 << 8));

        // 等待设备恢复 ~80ms
        self.spin_delay(20_000_000);

        // 读回速度
        let hprt = r.hprt0.get();
        self.speed = Some(speed::speed_from_hprt_bits(hprt >> 17));

        log::info!("DWC2: root port reset done, speed={:?}", self.speed);
        Ok(())
    }

    /// 分配一个主机通道。
    pub fn alloc_channel(
        &mut self,
        dev_addr: u8,
        ep_num: u8,
        ep_dir_in: bool,
        ep_type: EpType,
        mps: u16,
    ) -> Result<HostChannel, Error> {
        let low_speed = self.speed == Some(Speed::Low);

        // 通道 0 保留给 EP0 控制传输
        let start = if ep_type == EpType::Control { 0 } else { 1 };

        for n in start..16 {
            if self.channels[n].is_none() {
                let ch = HostChannel::new(
                    self.mmio, n as u8, dev_addr, ep_num, ep_dir_in, ep_type, mps, low_speed,
                );
                self.channels[n] = Some(ch);
                // 取走通道（Option take）
                return Ok(self.channels[n].take().unwrap());
            }
        }
        Err(Error::NoChannel)
    }

    /// 释放通道，归还到池。
    pub fn release_channel(&mut self, ch: HostChannel) {
        let num = ch.num() as usize;
        ch.release();
        self.channels[num] = None;
    }

    // ═══════════════════════════════════════════
    // EP0 控制传输便捷方法
    // ═══════════════════════════════════════════

    /// EP0 控制读（IN 数据阶段）。
    pub fn ep0_control_in(
        &mut self,
        dev_addr: u8,
        setup: &[u8; 8],
        buf: &mut [u8],
        ep0_mps: u16,
    ) -> Result<usize, Error> {
        // 取出 DMA 缓冲区避免借用冲突
        let mut dma = self.dma_buf.take().ok_or(Error::DmaTooSmall)?;
        let mut ch = self.alloc_channel(dev_addr, 0, false, EpType::Control, ep0_mps)?;
        let result = (|| {
            // SETUP 阶段：写 SETUP 到 DMA，clean cache 让 DMA 可见
            unsafe {
                let s = dma.slice_mut(0, 8);
                s.copy_from_slice(setup);
            }
            self.osal.dma_cache_clean(dma.va_ptr(), 8);
            ch.execute_with_retry(channel::PID_SETUP, 8, 1, dma.phys_at(0))?;

            // DATA IN 阶段（设备→主机）：单包循环 + 手动 toggle（匹配 sg200x-bsp）
            let mut received = 0usize;
            if !buf.is_empty() {
                ch.set_ep_dir(true);
                let mut toggle = channel::PID_DATA1;
                while received < buf.len() {
                    let chunk = ((buf.len() - received) as u32).min(ep0_mps as u32);
                    let n = ch.execute_with_retry(toggle, chunk, 1, dma.phys_at(0))?;
                    // invalidate cache: DMA 写了数据，CPU 需要看到
                    self.osal.dma_cache_invalidate(dma.va_ptr(), n);
                    unsafe {
                        let s = dma.slice(0, n);
                        let copy_len = n.min(buf.len() - received);
                        buf[received..received + copy_len].copy_from_slice(&s[..copy_len]);
                    }
                    received += n;
                    // 翻转 toggle（DATA1 ↔ DATA0）
                    toggle = if toggle == channel::PID_DATA1 {
                        channel::PID_DATA0
                    } else {
                        channel::PID_DATA1
                    };
                    // 短包 = 传输结束
                    if n < chunk as usize {
                        break;
                    }
                }
            }

            // STATUS OUT 阶段：切换回 EPDIR=0，永远用 PID_DATA1（USB spec 要求）
            ch.set_ep_dir(false);
            ch.execute_with_retry(channel::PID_DATA1, 0, 1, dma.phys_at(0))?;
            Ok(received)
        })();

        // 归还 DMA 缓冲区和通道
        self.release_channel(ch);
        self.dma_buf = Some(dma);
        result
    }

    /// EP0 控制写（OUT 数据阶段 + IN 状态）。
    pub fn ep0_control_out(
        &mut self,
        dev_addr: u8,
        setup: &[u8; 8],
        buf: &[u8],
        ep0_mps: u16,
    ) -> Result<(), Error> {
        let mut dma = self.dma_buf.take().ok_or(Error::DmaTooSmall)?;
        let mut ch = self.alloc_channel(dev_addr, 0, false, EpType::Control, ep0_mps)?;
        let result = (|| {
            // SETUP 阶段：clean cache 让 DMA 可见
            unsafe {
                let s = dma.slice_mut(0, 8);
                s.copy_from_slice(setup);
            }
            self.osal.dma_cache_clean(dma.va_ptr(), 8);
            ch.execute_with_retry(channel::PID_SETUP, 8, 1, dma.phys_at(0))?;

            // DATA OUT 阶段（主机→设备）：单包循环 + 手动 toggle（匹配 sg200x-bsp）
            if !buf.is_empty() {
                let mut toggle = channel::PID_DATA1;
                let mut offset = 0usize;
                while offset < buf.len() {
                    let chunk = ((buf.len() - offset) as u32).min(ep0_mps as u32);
                    unsafe {
                        let s = dma.slice_mut(0, chunk as usize);
                        s.copy_from_slice(&buf[offset..offset + chunk as usize]);
                    }
                    self.osal.dma_cache_clean(dma.va_ptr(), chunk as usize);
                    ch.execute_with_retry(toggle, chunk, 1, dma.phys_at(0))?;
                    offset += chunk as usize;
                    toggle = if toggle == channel::PID_DATA1 {
                        channel::PID_DATA0
                    } else {
                        channel::PID_DATA1
                    };
                }
            }

            // STATUS IN 阶段：切换 EPDIR=1（匹配 sg200x-bsp）
            ch.set_ep_dir(true);
            ch.execute_with_retry(channel::PID_DATA1, 0, 1, dma.phys_at(0))?;
            Ok(())
        })();

        self.release_channel(ch);
        self.dma_buf = Some(dma);
        result
    }

    /// EP0 控制传输（无数据阶段）。
    pub fn ep0_control_no_data(
        &mut self,
        dev_addr: u8,
        setup: &[u8; 8],
        ep0_mps: u16,
    ) -> Result<(), Error> {
        let mut dma = self.dma_buf.take().ok_or(Error::DmaTooSmall)?;
        let mut ch = self.alloc_channel(dev_addr, 0, false, EpType::Control, ep0_mps)?;
        let result = (|| {
            unsafe {
                let s = dma.slice_mut(0, 8);
                s.copy_from_slice(setup);
            }
            self.osal.dma_cache_clean(dma.va_ptr(), 8);
            ch.execute_with_retry(channel::PID_SETUP, 8, 1, dma.phys_at(0))?;
            // STATUS IN 阶段：切换 EPDIR=1
            ch.set_ep_dir(true);
            ch.execute_with_retry(channel::PID_DATA1, 0, 1, dma.phys_at(0))?;
            Ok(())
        })();

        self.release_channel(ch);
        self.dma_buf = Some(dma);
        result
    }

    /// 获取 osal 引用（供 adapter 层使用）。
    #[inline]
    pub fn osal(&self) -> &'static dyn Osal {
        self.osal
    }

    // ═══════════════════════════════════════════
    // 内部初始化函数
    // ═══════════════════════════════════════════

    fn spin_delay(&self, n: u32) {
        for _ in 0..n {
            core::hint::spin_loop();
        }
    }

    fn core_soft_reset(&self) -> Result<(), Error> {
        self.wait_ahb_idle()?;
        let r = self.mmio.regs();
        let snpsid = r.gsnpsid.get();
        let core_rev = snpsid & DWC2_CORE_REV_MASK;
        let new_rst_seq = core_rev >= (DWC2_CORE_REV_4_20A & DWC2_CORE_REV_MASK);

        r.grstctl.modify(GRSTCTL::CSFTRST::SET);

        if !new_rst_seq {
            for _ in 0..3_000_000u32 {
                if !r.grstctl.is_set(GRSTCTL::CSFTRST) {
                    self.spin_delay(4096);
                    return Ok(());
                }
                self.spin_delay(32);
            }
            return Err(Error::Timeout);
        }

        // Core ≥ 4.20a: 等 CSFTRST_DONE
        for _ in 0..3_000_000u32 {
            if r.grstctl.is_set(GRSTCTL::CSFTRST_DONE) {
                r.grstctl
                    .modify(GRSTCTL::CSFTRST::CLEAR + GRSTCTL::CSFTRST_DONE::SET);
                self.spin_delay(4096);
                return Ok(());
            }
            self.spin_delay(32);
        }
        Err(Error::Timeout)
    }

    fn wait_ahb_idle(&self) -> Result<(), Error> {
        let r = self.mmio.regs();
        for _ in 0..3_000_000u32 {
            if r.grstctl.is_set(GRSTCTL::AHBIDLE) {
                return Ok(());
            }
            self.spin_delay(32);
        }
        Err(Error::Timeout)
    }

    fn force_host_mode(&self) -> Result<(), Error> {
        let r = self.mmio.regs();
        r.gusbcfg.modify(GUSBCFG::FORCEHOSTMODE::SET);
        self.spin_delay(100_000);
        for _ in 0..500_000u32 {
            if r.gintsts.is_set(GINTSTS::CURMODE_HOST) {
                return Ok(());
            }
            self.spin_delay(32);
        }
        Err(Error::Other("CURMODE_HOST not set"))
    }

    fn init_gotgctl_override(&self) {
        self.mmio.regs().gotgctl.modify(
            GOTGCTL::DBNCE_FLTR_BYPASS::SET
                + GOTGCTL::AVALOEN::SET
                + GOTGCTL::AVALOVAL::SET
                + GOTGCTL::VBVALOEN::SET
                + GOTGCTL::VBVALOVAL::SET,
        );
        self.spin_delay(200_000);
    }

    fn init_gusbcfg(&self) {
        let r = self.mmio.regs();
        let utmi_w = r.ghwcfg4.read(GHWCFG4::UTMI_PHY_DATA_WIDTH);
        let want_16bit = utmi_w == 1; // 16-bit only
        let mut field =
            GUSBCFG::FORCEHOSTMODE::SET + GUSBCFG::ULPI_UTMI_SEL::CLEAR + GUSBCFG::TOUTCAL.val(0x7);
        if want_16bit {
            field += GUSBCFG::PHYIF16::SET;
        }
        r.gusbcfg.modify(field);
    }

    fn init_gahb(&self) {
        let r = self.mmio.regs();
        let arch = r.ghwcfg2.read(GHWCFG2::ARCH);
        r.gahbcfg
            .modify(GAHBCFG::HBSTLEN::Incr16 + GAHBCFG::GLBL_INTR_EN::SET);
        if arch == 2 {
            r.gahbcfg.modify(GAHBCFG::DMA_EN::SET);
        }
    }

    fn init_hcfg(&self) {
        // HS 强制：不置 FSLSSUPP
        self.mmio
            .regs()
            .hcfg
            .modify(HCFG::FSLSSUPP::CLEAR + HCFG::FSLSPCLKSEL.val(0));
    }

    fn init_fifos(&self) -> Result<(), Error> {
        let r = self.mmio.regs();
        let total = r.ghwcfg3.read(GHWCFG3::DFIFO_DEPTH);
        let hc = 1 + r.ghwcfg2.read(GHWCFG2::NUM_HOST_CHAN);

        let mut rx: u32 = 536;
        let mut nptx: u32 = 32;
        let mut ptx: u32 = 768;

        if rx.saturating_add(nptx).saturating_add(ptx) > total {
            rx = 516 + hc;
            nptx = 256;
            ptx = 768;
        }
        let sum = rx.saturating_add(nptx).saturating_add(ptx);
        if sum > total {
            ptx = total.saturating_sub(rx).saturating_sub(nptx);
        }

        r.grxfsiz.set(rx & 0xffff);
        r.gnptxfsiz
            .set(((nptx << 16) & 0xffff_0000) | (rx & 0xffff));
        r.hptxfsiz
            .set(((ptx << 16) & 0xffff_0000) | ((rx + nptx) & 0xffff));

        let snpsid = r.gsnpsid.get();
        let ded = r.ghwcfg4.is_set(GHWCFG4::DED_FIFO_EN);
        if ded && snpsid >= DWC2_CORE_REV_2_91A {
            let epbase = rx.wrapping_add(nptx).wrapping_add(ptx);
            r.gdfifocfg.modify(GDFIFOCFG::EPINFOBASE.val(epbase));
        }

        Ok(())
    }

    fn flush_rx_fifo(&self) -> Result<(), Error> {
        self.wait_ahb_idle()?;
        let r = self.mmio.regs();
        r.grstctl.write(GRSTCTL::RXFFLSH::SET);
        self.wait_grstctl_handshake(GRSTCTL::RXFFLSH, false)?;
        self.spin_delay(2000);
        Ok(())
    }

    fn flush_tx_fifo_all(&self) -> Result<(), Error> {
        self.wait_ahb_idle()?;
        let r = self.mmio.regs();
        r.grstctl
            .write(GRSTCTL::TXFFLSH::SET + GRSTCTL::TXFNUM.val(0x10));
        self.wait_grstctl_handshake(GRSTCTL::TXFFLSH, false)?;
        self.spin_delay(2000);
        Ok(())
    }

    fn wait_grstctl_handshake(
        &self,
        field: tock_registers::fields::Field<u32, GRSTCTL::Register>,
        set: bool,
    ) -> Result<(), Error> {
        let r = self.mmio.regs();
        for _ in 0..3_000_000u32 {
            if r.grstctl.is_set(field) == set {
                self.spin_delay(64);
                return Ok(());
            }
            self.spin_delay(8);
        }
        Err(Error::Timeout)
    }

    fn port_power_on(&self) {
        let r = self.mmio.regs();
        let cur = r.hprt0.get() & !HPRT0_W1C_MASK;
        r.hprt0.set(cur | (1 << 12));
    }

    fn clear_utmi_override(&self) {
        // CV182x PHY UTMI override clear
        // 此操作在 SG2002 上是必需的 — 将 PHY 控制权还给 DWC2
        // 具体实现由 platform init 处理
    }
}
