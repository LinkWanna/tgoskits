//! DMA 缓冲区抽象。
//!
//! 提供 VA/PA 对，用于 DWC2 内部 DMA 引擎的 HCDMA 寄存器写入。
//! 当前使用全局共享缓冲区（单传输模型，无并发需求）。

/// DMA 缓冲区 — 持有虚拟地址和物理地址的配对。
///
/// CPU 通过 `as_slice()`/`as_mut_slice()` 访问虚拟地址；
/// DWC2 硬件通过 `phys()` 获取物理地址写入 HCDMA。
#[derive(Debug)]
pub struct DmaBuffer {
    /// CPU 可访问的虚拟地址
    va: *mut u8,
    /// 总线物理地址（供 DWC2 内部 DMA 使用）
    pa: u32,
    /// 缓冲区大小（字节）
    size: usize,
}

// DmaBuffer 可以在线程间传递（裸指针由调用者保证安全）
unsafe impl Send for DmaBuffer {}

impl DmaBuffer {
    /// 从原始指针创建 DMA 缓冲区。
    ///
    /// # Safety
    ///
    /// `va` 必须指向至少 `size` 字节的有效内存，
    /// `pa` 必须是对应的总线物理地址。
    pub unsafe fn from_raw(va: *mut u8, pa: u32, size: usize) -> Self {
        Self { va, pa, size }
    }

    /// 缓冲区大小。
    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }

    /// 物理地址。
    #[inline]
    pub fn phys(&self) -> u32 {
        self.pa
    }

    /// 虚拟地址裸指针（供适配层直接访问 DMA 缓冲区内存）。
    #[inline]
    pub fn va_ptr(&self) -> *mut u8 {
        self.va
    }

    /// 以指定偏移获取物理地址。
    ///
    /// # Panics
    ///
    /// 如果 `offset > size`。
    #[inline]
    pub fn phys_at(&self, offset: usize) -> u32 {
        assert!(offset <= self.size);
        self.pa + offset as u32
    }

    /// 不可变字节切片视图。
    ///
    /// # Safety
    ///
    /// 调用者保证在切片生命周期内没有并发可变访问。
    #[inline]
    pub unsafe fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.va, self.size) }
    }

    /// 可变字节切片视图。
    ///
    /// # Safety
    ///
    /// 调用者保证在切片生命周期内没有并发访问。
    #[inline]
    pub unsafe fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.va, self.size) }
    }

    /// 获取从 `offset` 开始的子视图。
    ///
    /// # Safety
    ///
    /// `offset + len <= size` 由调用者保证（否则 panic）。
    #[inline]
    pub unsafe fn slice(&self, offset: usize, len: usize) -> &[u8] {
        unsafe {
            assert!(offset + len <= self.size);
            core::slice::from_raw_parts(self.va.add(offset), len)
        }
    }

    /// 获取从 `offset` 开始的可变子视图。
    #[inline]
    pub unsafe fn slice_mut(&mut self, offset: usize, len: usize) -> &mut [u8] {
        unsafe {
            assert!(offset + len <= self.size);
            core::slice::from_raw_parts_mut(self.va.add(offset), len)
        }
    }
}

impl Drop for DmaBuffer {
    fn drop(&mut self) {
        // DMA 缓冲区的释放由 Osal::dma_free 处理；
        // DmaBuffer 自身不释放，仅持有指针。
    }
}
