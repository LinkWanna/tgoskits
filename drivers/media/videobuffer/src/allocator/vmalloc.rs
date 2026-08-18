//! VirtualAllocator——实现 Vb2MemOps 的 vmalloc 风格内核页分配器。
//!
//! 对齐 Linux `vb2_vmalloc`：**虚拟连续、物理离散**——在内核地址空间
//! （`axmm::kernel_aspace`）分配连续虚拟段，逐页从 axalloc 堆分配物理
//! 帧并 `map_linear` 建立映射（不要求物理连续大块，长时运行系统不易
//! 碎片失败）。
//!
//! 物理页快照由分配器自管（`alloc` 时逐页分配并记录）——不需要页表
//! 查询。注意 `virt_to_phys` 只对**线性映射区**有效（ax-plat mem 契约），
//! 不能翻译 vmalloc 段任意虚拟地址——所以物理页从 axalloc 堆分配
//! （其 vaddr 在线性映射区，换算有效），再映射到 vmalloc 段。
//!
//! 布局完全内聚：`alloc` 时计算 UAPI mmap 偏移（stride）；`cookie` =
//! 虚拟段基址（CPU 直写）。供 vivid 及 CPU 搬运场景（media-uvc 拼帧）。

use alloc::vec::Vec;

use ax_alloc::{UsageKind, global_allocator};
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange, align_up_4k};
use ax_mm::kernel_aspace;
use ax_runtime::hal::{
    mem::{phys_to_virt, virt_to_phys},
    paging::MappingFlags,
};
use v4l2_core::V4l2Error;

use crate::{MemPlane, Vb2MemOps};

/// 跟踪一个已分配 buffer 的虚拟段与物理页（自管，release 时归还）。
struct AllocEntry {
    /// 虚拟段基址（cookie 也取此值——CPU 直写地址）。
    vaddr: usize,
    size: usize,
    /// 逐页物理地址（axalloc 堆分配；unmap/归还前不变）。
    pages: Vec<usize>,
}

/// 用于 V4L2 buffer 内存的 vmalloc 风格 allocator。
///
/// 分配流程（每次 `alloc`）：在内核地址空间中找空闲虚拟段
/// （4K 对齐）→ 逐页 `global_allocator().alloc_pages(1, 4K)` 分配物理
/// 帧（vaddr 在线性映射区，`virt_to_phys` 换算有效）→ `map_linear`
/// 逐页映射到虚拟段（虚拟连续、物理离散）。`mmap` 偏移按 stride
/// （页对齐 plane 大小）在 alloc 时计算，buffer 间不重叠。
///
/// 线程安全：所有方法都接受 `&self`。内部状态由
/// `SpinLock` 保护——采集路径（media-uvc 拼帧）与 ioctl 路径
/// 均为任务上下文，纯自旋锁即可。`kernel_aspace` 内部同样是
/// IRQ 安全锁。
pub struct VirtualAllocator {
    entries: ax_sync::SpinLock<Vec<AllocEntry>>,
}

impl VirtualAllocator {
    pub fn new() -> Self {
        Self {
            entries: ax_sync::SpinLock::new(Vec::new()),
        }
    }
}

impl Default for VirtualAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl Vb2MemOps for VirtualAllocator {
    fn alloc(&self, sizes: &[u32]) -> Result<Vec<MemPlane>, V4l2Error> {
        let mut entries = self.entries.lock();
        let mut planes = Vec::with_capacity(sizes.len());
        let mut aspace = kernel_aspace().lock();

        // 单 plane 语义：stride = 页对齐 plane 大小；每 buffer 一个 entry。
        let stride = align_up_4k(sizes.first().copied().unwrap_or(0) as usize);
        let buf_index = entries.len();

        for (p, &size) in sizes.iter().enumerate() {
            if size == 0 {
                return Err(V4l2Error::InvalidArgument);
            }
            let aligned = align_up_4k(size as usize);
            // 内核地址空间内找空闲虚拟段（4K 对齐）。
            let limit = VirtAddrRange::new(aspace.base(), aspace.end());
            let start = aspace
                .find_free_area(aspace.base(), aligned, limit)
                .ok_or(V4l2Error::NoMemory)?;

            // 逐页分配物理帧（axalloc 堆——vaddr 在线性映射区，
            // virt_to_phys 换算有效）并映射到虚拟段。
            let mut pages = Vec::with_capacity(aligned / PAGE_SIZE_4K);
            for i in 0..aligned / PAGE_SIZE_4K {
                let vaddr = global_allocator()
                    .alloc_pages(1, PAGE_SIZE_4K, UsageKind::VirtMem)
                    .map_err(|_| V4l2Error::NoMemory)?;
                let pa = virt_to_phys(VirtAddr::from(vaddr)).as_usize();
                aspace
                    .map_linear(
                        start + i * PAGE_SIZE_4K,
                        PhysAddr::from_usize(pa),
                        PAGE_SIZE_4K,
                        MappingFlags::READ | MappingFlags::WRITE,
                    )
                    .map_err(|_| V4l2Error::NoMemory)?;
                pages.push(pa);
            }

            let vaddr = start.as_usize();
            if p == 0 {
                entries.push(AllocEntry {
                    vaddr,
                    size: aligned,
                    pages,
                });
            }
            // 多 plane 时仅第一个 plane 记录 entry（单 plane 场景未用）；
            // 布局：buffer 内 plane 顺序排列（当前单 plane，offset 即 buffer 起点）。
            planes.push(MemPlane {
                cookie: vaddr,
                offset: (buf_index * stride) as usize,
                // 记录页对齐后的实际大小：用户态 mmap 的 length 是页对齐的，
                // 若记录未对齐的 size 会导致 mmap 越界检查（offset+length>end）
                // 误拒绝（Linux vb2 的 plane.length 同样是页对齐值）。
                length: aligned as u32,
            });
        }
        Ok(planes)
    }

    fn release(&self, planes: &[MemPlane]) {
        let mut entries = self.entries.lock();
        let mut aspace = kernel_aspace().lock();
        for plane in planes {
            // 匹配后立即移除 entry：entries 只保留活跃 buffer，同一 vaddr 至多
            // 一个 entry。曾长期不删除——下次 alloc 时 find_free_area 复用刚
            // unmap 的段地址，新 entry 与旧 entry vaddr 冲突，mmap/release 的
            // find 命中旧 entry → 返回已释放/错页数的物理页（板上实测 pages
            // short → mmap EINVAL，及 use-after-free 隐患）。
            if let Some(pos) = entries.iter().position(|e| e.vaddr == plane.cookie) {
                let e = entries.swap_remove(pos);
                // 先摘映射（PTE），再归还物理帧。
                aspace.unmap(VirtAddr::from(e.vaddr), e.size).ok();
                for pa in &e.pages {
                    let vaddr = phys_to_virt(PhysAddr::from_usize(*pa));
                    global_allocator().dealloc_pages(vaddr.as_usize(), 1, UsageKind::VirtMem);
                }
            }
        }
    }

    fn mmap(&self, plane: &MemPlane) -> Vec<usize> {
        let entries = self.entries.lock();
        entries
            .iter()
            .find(|e| e.vaddr == plane.cookie)
            .map(|e| e.pages.clone())
            .unwrap_or_default()
    }
}
