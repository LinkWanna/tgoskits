//! DWC2 端点 — 封装一个 HostChannel + DMA 缓冲区。
//!
//! 提供 `submit()` 方法，接收 `TransferRequest` 并转换为 DWC2 通道传输。

use crate::{
    channel::{EpType, HostChannel},
    dma::DmaBuffer,
    err::Error,
};

/// DWC2 端点。
///
/// 持有已分配的主机通道和 DMA 缓冲区引用。
/// 每次 `submit()` 使用通道执行一次传输后立即释放通道，
/// 或者对于需要多次传输的场景，可以保持通道绑定。
pub struct Dwc2Endpoint {
    /// 端点地址（bit 7 = 方向）
    ep_addr: u8,
    /// 端点类型
    ep_type: EpType,
    /// 最大包大小
    mps: u16,
    /// 设备地址
    dev_addr: u8,
    /// 是否低速设备
    low_speed: bool,
}

impl Dwc2Endpoint {
    pub fn new(ep_addr: u8, ep_type: EpType, mps: u16, dev_addr: u8, low_speed: bool) -> Self {
        Self {
            ep_addr,
            ep_type,
            mps,
            dev_addr,
            low_speed,
        }
    }

    /// 端点地址。
    #[inline]
    pub fn address(&self) -> u8 {
        self.ep_addr
    }

    /// 端点类型。
    #[inline]
    pub fn ep_type(&self) -> EpType {
        self.ep_type
    }

    /// 分配通道并执行一次 Bulk IN 传输。
    pub fn bulk_in(
        &self,
        dma_buf: &mut DmaBuffer,
        buf: &mut [u8],
        alloc_ch: impl FnOnce(u8, u8, bool, EpType, u16) -> Result<HostChannel, Error>,
    ) -> Result<usize, Error> {
        let ep_num = self.ep_addr & 0x7F;
        let mut ch = alloc_ch(self.dev_addr, ep_num, true, self.ep_type, self.mps)?;

        // 将 DMA 缓冲区清零
        let xfer_len = buf.len().min(dma_buf.size());
        unsafe {
            let s = dma_buf.slice_mut(0, xfer_len);
            s[..xfer_len].fill(0);
        }

        let actual = ch.execute_with_retry(0, buf.len() as u32, 1, dma_buf.phys_at(0));

        // 拷贝结果回用户 buffer
        if let Ok(n) = actual {
            unsafe {
                let s = dma_buf.slice(0, n);
                let len = s.len().min(buf.len());
                buf[..len].copy_from_slice(&s[..len]);
            }
        }

        ch.release();
        actual
    }

    /// 分配通道并执行一次 Bulk OUT 传输。
    pub fn bulk_out(
        &self,
        dma_buf: &mut DmaBuffer,
        buf: &[u8],
        alloc_ch: impl FnOnce(u8, u8, bool, EpType, u16) -> Result<HostChannel, Error>,
    ) -> Result<usize, Error> {
        let ep_num = self.ep_addr & 0x7F;
        let mut ch = alloc_ch(self.dev_addr, ep_num, false, self.ep_type, self.mps)?;

        // 拷贝数据到 DMA 缓冲区
        unsafe {
            let s = dma_buf.slice_mut(0, buf.len().min(dma_buf.size()));
            s[..buf.len()].copy_from_slice(buf);
        }

        let actual = ch.execute_with_retry(0, buf.len() as u32, 1, dma_buf.phys_at(0));

        ch.release();
        actual
    }

    /// 同步传输 IN（单微帧）。
    ///
    /// 正确解码 ISOC wMaxPacketSize：bits[12:11]=mult-1, bits[10:0]=packet_bytes。
    /// 匹配 sg200x-bsp 的 `isoch_in_uframe`。
    pub fn isoch_in_uframe(
        &self,
        dma_buf: &mut DmaBuffer,
        buf: &mut [u8],
        alloc_ch: impl FnOnce(u8, u8, bool, EpType, u16) -> Result<HostChannel, Error>,
    ) -> Result<usize, Error> {
        let ep_num = self.ep_addr & 0x7F;

        // 解码 ISOC wMaxPacketSize（匹配 sg200x-bsp）
        let mps_raw = self.mps;
        let tx_bytes = (mps_raw & 0x7FF) as u32; // 每事务字节数
        let mult = ((mps_raw >> 11) & 0x3) as u32 + 1; // 每微帧事务数 (1-3)
        if tx_bytes == 0 || mult > 3 {
            return Err(Error::InvalidParam);
        }

        let max_uframe = (tx_bytes * mult) as usize;
        let xfer = buf.len().min(max_uframe).min(dma_buf.size());

        // MPS 传给通道初始化用事务级（不包含 mult 编码）
        let mut ch = alloc_ch(self.dev_addr, ep_num, true, self.ep_type, tx_bytes as u16)?;

        unsafe {
            let s = dma_buf.slice_mut(0, xfer);
            s[..xfer].fill(0);
        }

        // PID 和 PKTCNT 匹配 mult（sg200x-bsp 逻辑）
        let (pid, pktcnt) = match mult {
            3 => (crate::channel::PID_DATA2, 3u32),
            2 => (crate::channel::PID_DATA1, 2u32),
            _ => (0u32, 1u32), // PID_DATA0
        };

        let actual = match ch.execute_with_retry(pid, xfer as u32, pktcnt, dma_buf.phys_at(0)) {
            Ok(n) => {
                unsafe {
                    let s = dma_buf.slice(0, n);
                    let len = s.len().min(buf.len());
                    buf[..len].copy_from_slice(&s[..len]);
                }
                Ok(n)
            }
            Err(Error::NakExhausted) => {
                // isoch 没有 NAK 重试 — NAK 意味着该微帧无数据
                Ok(0)
            }
            Err(e) => Err(e),
        };

        ch.release();
        actual
    }
}
