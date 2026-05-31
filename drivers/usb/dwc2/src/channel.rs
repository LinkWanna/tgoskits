//! DWC2 主机通道管理。
//!
//! DWC2 主机通道管理。
//!
//! 每个 `HostChannel` 绑定一个硬件通道（0..15），负责：
//! - 通道分配与释放
//! - HCCHAR/HCTSIZ/HCDMA 编程
//! - 传输执行与等待
//! - NAK/STALL/XACTERR 处理

use tock_registers::interfaces::{Readable, Writeable};

use crate::{
    err::Error,
    mmio::Dwc2Mmio,
    reg::{HCCHAR, HCINT},
};

// ── HCINT 全清掩码 ──
const HCINT_ALL_W1C: u32 = 0x7FF;

// ── HCCHAR 原始位 ──
const HCCHAR_CHENA: u32 = 1 << 31;
const HCCHAR_CHDIS: u32 = 1 << 30;
const HCCHAR_ODDFRM: u32 = 1 << 29;
#[allow(dead_code)]
const HCCHAR_EPDIR: u32 = 1 << 15;
#[allow(dead_code)]
const HCCHAR_EPTYPE_CONTROL: u32 = 0 << 18;
const HCCHAR_EPTYPE_ISOCH: u32 = 1 << 18;
const HCCHAR_EPTYPE_BULK: u32 = 2 << 18;
const HCCHAR_EPTYPE_INTERRUPT: u32 = 3 << 18;
#[allow(dead_code)]
const HCCHAR_MC_SHIFT: u32 = 20;

// ── HCTSIZ PID（公开给 controller 使用）──
pub const PID_DATA0: u32 = 0;
#[allow(dead_code)]
pub const PID_DATA2: u32 = 1;
pub const PID_DATA1: u32 = 2;
pub const PID_SETUP: u32 = 3;

/// 重试次数。
const NAK_RETRIES: u32 = 64;
const XACT_RETRIES: u32 = 8;

/// DWC2 主机通道。
///
/// 分配后绑定到一个硬件通道号，持有 MMIO 引用。
/// 释放时需要调用 `release()` 让通道回到空闲池。
pub struct HostChannel {
    /// 通道号（0..15）
    num: u8,
    /// 设备地址
    dev_addr: u8,
    /// 端点号（不含方向位）
    ep_num: u8,
    /// 端点类型
    ep_type: EpType,
    /// 最大包大小
    mps: u16,
    /// 数据翻转位（仅 Control/Bulk/Interrupt）
    data_toggle: bool,
    /// 是否低速设备
    low_speed: bool,
    /// MMIO 视图
    mmio: &'static Dwc2Mmio,
}

/// 端点类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpType {
    Control     = 0,
    Isochronous = 1,
    Bulk        = 2,
    Interrupt   = 3,
}

impl EpType {
    fn hcchar_bits(self) -> u32 {
        match self {
            EpType::Control => HCCHAR_EPTYPE_CONTROL,
            EpType::Isochronous => HCCHAR_EPTYPE_ISOCH,
            EpType::Bulk => HCCHAR_EPTYPE_BULK,
            EpType::Interrupt => HCCHAR_EPTYPE_INTERRUPT,
        }
    }

    fn pid_for_data_toggle(self, toggle: bool) -> u32 {
        match self {
            EpType::Control => {
                // 控制传输 PID 由 SETUP/DATA/STATUS 阶段分别指定
                // 这里仅用于 DATA 阶段
                if toggle { PID_DATA1 } else { PID_DATA0 }
            }
            _ => {
                if toggle {
                    PID_DATA1
                } else {
                    PID_DATA0
                }
            }
        }
    }
}

impl HostChannel {
    /// 分配一个主机通道。
    ///
    /// 配置 HCCHAR 的基本字段（MPS、EP、方向、类型、设备地址、低速标志）。
    /// 不立即使能通道 — 等 `execute_transfer()` 时完成 HCTSIZ/HCDMA/HCCHAR.CHENA。
    pub fn new(
        mmio: &'static Dwc2Mmio,
        num: u8,
        dev_addr: u8,
        ep_num: u8,
        ep_dir_in: bool,
        ep_type: EpType,
        mps: u16,
        low_speed: bool,
    ) -> Self {
        // 预计算 HCCHAR 基础字段（不含 CHENA/CHDIS）
        let mut hcchar_base = ep_type.hcchar_bits();
        hcchar_base |= (mps as u32) & 0x7FF; // MPS
        hcchar_base |= (ep_num as u32 & 0xF) << 11; // EPNUM
        if ep_dir_in {
            hcchar_base |= HCCHAR_EPDIR;
        }
        hcchar_base |= (dev_addr as u32 & 0x7F) << 22; // DEVADDR
        if low_speed {
            hcchar_base |= 1 << 17; // LSPDDEV
        }
        // 对于 isoch/interrupt，MC=1（每(微)帧 1 事务）
        if matches!(ep_type, EpType::Isochronous | EpType::Interrupt) {
            hcchar_base |= 1 << HCCHAR_MC_SHIFT;
        }

        // 写 HCCHAR 基础配置（不合 CHENA）
        let ch = mmio.host_channel(num as usize);
        ch.hcchar.set(hcchar_base);

        Self {
            num,
            dev_addr,
            ep_num,
            ep_type,
            mps,
            data_toggle: false,
            low_speed,
            mmio,
        }
    }

    /// 通道号。
    #[inline]
    pub fn num(&self) -> u8 {
        self.num
    }

    /// 设置 EPDIR（IN/OUT 方向）。
    /// 用于控制传输的 DATA/STATUS 阶段切换方向。
    pub fn set_ep_dir(&mut self, dir_in: bool) {
        let ch = self.mmio.host_channel(self.num as usize);
        let mut v = ch.hcchar.get();
        if dir_in {
            v |= HCCHAR_EPDIR;
        } else {
            v &= !HCCHAR_EPDIR;
        }
        ch.hcchar.set(v);
    }

    /// 执行一次传输。
    ///
    /// - `pid`: HCTSIZ.PID 值（DATA0/DATA1/SETUP）
    /// - `xfer_size`: 传输字节数（写入 HCTSIZ.XFERSIZE）
    /// - `pkt_count`: 包数量（写入 HCTSIZ.PKTCNT；control/bulk 通常为 1）
    /// - `dma_phys`: DMA 物理地址（写入 HCDMA）
    ///
    /// 返回实际传输字节数（从 HCTSIZ 读回剩余量反算）。
    /// 不修改 `data_toggle` — 由调用者管理翻转。
    pub fn execute(
        &mut self,
        pid: u32,
        xfer_size: u32,
        pkt_count: u32,
        dma_phys: u32,
    ) -> Result<usize, Error> {
        let ch = self.mmio.host_channel(self.num as usize);

        // 等待通道空闲
        self.wait_disabled()?;
        self.halt_channel();

        // 清中断
        ch.hcint.set(HCINT_ALL_W1C);
        ch.hcsplt.set(0);

        // 写 HCTSIZ: XFERSIZE + PKTCNT + PID
        let hctsiz = (xfer_size & 0x7FFFF) | ((pkt_count & 0x3FF) << 19) | ((pid & 3) << 29);
        ch.hctsiz.set(hctsiz);

        // 内存屏障（RISC-V）
        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }

        // 写 HCDMA
        ch.hcdma.set(dma_phys);

        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
        }

        // ISOC：读 HFNUM 动态设置 OddFrm（匹配 sg200x-bsp）
        let hcchar = if matches!(self.ep_type, EpType::Isochronous) {
            let frnum = self.mmio.regs().hfnum.get() & 0xFFFF;
            let oddfrm = if (frnum & 1) == 0 { HCCHAR_ODDFRM } else { 0 };
            ch.hcchar.get() | oddfrm
        } else {
            ch.hcchar.get()
        };

        log::trace!(
            "DWC2 ch{} xfer: HCCHAR(read)={:#010x} HCTSIZ={:#010x} HCDMA={:#010x} pid={} size={}",
            self.num,
            hcchar,
            hctsiz,
            dma_phys,
            pid,
            xfer_size
        );

        // 只写 CHENA（不设 CHDIS — PKTCNT=1 自动 halt，BSP 验证可行）
        ch.hcchar.set(hcchar | HCCHAR_CHENA);

        // 等待 CHHLTD
        let hi = self.wait_halted()?;

        log::trace!(
            "DWC2 ch{} done: HCINT={:#010x} HCTSIZ(remaining)={}",
            self.num,
            hi.get(),
            ch.hctsiz.get() & 0x7FFFF
        );

        // 检查结果
        if hi.is_set(HCINT::STALL) {
            let hprt = self.mmio.regs().hprt0.get();
            log::warn!(
                "DWC2 ch{} STALL: HCINT={:#010x} HCCHAR={:#010x} HPRT0={:#010x}",
                self.num,
                hi.get(),
                ch.hcchar.get(),
                hprt
            );
            return Err(Error::Stall);
        }
        if hi.is_set(HCINT::XACTERR) {
            return Err(Error::Transfer);
        }
        if hi.is_set(HCINT::NAK) {
            return Err(Error::NakExhausted);
        }
        // For data phases: if XFERCOMPL not set, retry (matching sg200x-bsp).
        // SETUP/STATUS phases don't set XFERCOMPL — skip check.
        if pid != PID_SETUP && xfer_size > 0 && !hi.is_set(HCINT::XFERCOMPL) {
            return Err(Error::NakExhausted);
        }

        // 实际传输量 = 请求量 - 剩余量
        let remaining = ch.hctsiz.get() & 0x7FFFF;
        let actual = xfer_size.saturating_sub(remaining) as usize;
        Ok(actual)
    }

    /// 执行一次带 NAK/XACTERR 重试的传输（EP0 控制传输使用）。
    pub fn execute_with_retry(
        &mut self,
        pid: u32,
        xfer_size: u32,
        pkt_count: u32,
        dma_phys: u32,
    ) -> Result<usize, Error> {
        let mut xact_left = XACT_RETRIES;
        for nak_attempt in 0..=NAK_RETRIES {
            match self.execute(pid, xfer_size, pkt_count, dma_phys) {
                Ok(n) => return Ok(n),
                Err(Error::Stall) => return Err(Error::Stall),
                Err(Error::NakExhausted) => {
                    if nak_attempt == NAK_RETRIES {
                        return Err(Error::NakExhausted);
                    }
                    self.spin_delay(200_000); // ~1ms backoff
                    continue;
                }
                Err(Error::Transfer) => {
                    if xact_left == 0 {
                        return Err(Error::Transfer);
                    }
                    xact_left -= 1;
                    self.spin_delay(2_000_000); // ~1ms for bus to settle
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Err(Error::NakExhausted)
    }

    /// 释放通道，归还到空闲池。
    pub fn release(self) {
        // 确保通道停止
        self.halt_channel();
        self.wait_disabled_quiet();
        // self 被 drop，通道号回到空闲池
    }

    // ── 内部辅助 ──

    fn spin_delay(&self, n: u32) {
        for _ in 0..n {
            core::hint::spin_loop();
        }
    }

    fn wait_disabled(&self) -> Result<(), Error> {
        let ch = self.mmio.host_channel(self.num as usize);
        for _ in 0..2_000_000u32 {
            if !ch.hcchar.is_set(HCCHAR::CHENA) {
                return Ok(());
            }
            self.spin_delay(8);
        }
        Err(Error::Timeout)
    }

    fn wait_disabled_quiet(&self) {
        let ch = self.mmio.host_channel(self.num as usize);
        for _ in 0..2_000_000u32 {
            if !ch.hcchar.is_set(HCCHAR::CHENA) {
                return;
            }
            self.spin_delay(8);
        }
    }

    fn halt_channel(&self) {
        let ch = self.mmio.host_channel(self.num as usize);
        let v = ch.hcchar.get();
        if v & HCCHAR_CHENA == 0 {
            return;
        }
        ch.hcchar.set(v | HCCHAR_CHENA | HCCHAR_CHDIS);
        for _ in 0..500_000u32 {
            if !ch.hcchar.is_set(HCCHAR::CHENA) {
                return;
            }
            self.spin_delay(8);
        }
    }

    fn wait_halted(
        &self,
    ) -> Result<tock_registers::LocalRegisterCopy<u32, HCINT::Register>, Error> {
        let ch = self.mmio.host_channel(self.num as usize);
        for _ in 0..8_000_000u32 {
            let hi = ch.hcint.extract();
            if hi.is_set(HCINT::CHHLTD) {
                ch.hcint.set(hi.get());
                return Ok(hi);
            }
            self.spin_delay(8);
        }
        Err(Error::Timeout)
    }
}
