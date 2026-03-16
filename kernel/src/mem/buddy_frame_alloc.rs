use super::MemRegion;
use super::PAGE_SIZE;
use crate::mem::allocator::buddy_alloc::MAX_ORDER_SIZE;
use crate::mem::high_order_alloc::FreeMem;
use core::alloc::{AllocError, Layout};
use x86_64::PhysAddr;
use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size1GiB, Size2MiB, Size4KiB};

pub use pre_init_framealloc::drain_map;

type BackedConfig = bootstrappable_buddy_allocator::BuddyAllocator<
    ORDERS,
    PAGE_SIZE_OFFSET,
    bootstrappable_buddy_allocator::Overflow,
    u64,
    memory_addresses::PhysAddr,
    alloc::alloc::Global,
>;

const PAGE_SIZE_OFFSET: usize = PAGE_SIZE.trailing_zeros() as usize;
const ORDERS: usize = 9;
const ORDER_MAX_SIZE: usize = BackedConfig::ORDER_MAX_SIZE;
/// ratio of [ORDER_MAX_SIZE] to [HIGH_ORDER_BLOCK_SIZE]
const HIGH_ORDER_BLOCK_RATIO: u32 = 2;
const HIGH_ORDER_BLOCK_SIZE: u32 = (ORDER_MAX_SIZE as u32) * HIGH_ORDER_BLOCK_RATIO;

static MEM_MAP: crate::util::KernelStatic<pre_init_framealloc::PreInitFrameAlloc> =
    crate::util::KernelStatic::new();

pub(super) unsafe fn init_mem_map(regions: hatcher::boot_info::MemoryMap) {
    for i in regions.iter() {
        crate::serial_println!("{:x?}", i)
    }
    MEM_MAP.init(unsafe { pre_init_framealloc::PreInitFrameAlloc::new(regions) });
}

mod pre_init_framealloc {
    use crate::mem::MemRegion;
    use crate::mem::buddy_frame_alloc::MEM_MAP;
    use hatcher::boot_info::MemoryMap;
    use x86_64::PhysAddr;
    use x86_64::structures::paging::{PhysFrame, Size4KiB};

    pub fn drain_map() {
        let mut map = MEM_MAP.get();
        let mut phys_alloc = super::super::allocator::PHYS_ALLOCATOR.lock();

        // SAFETY: This is not safe. This should be reviewed if changes are made to PreInitFrameAlloc
        // This unbinds the lifetime of map.map from `self. We need to do this because drain takes
        // `&mut self` which prevents us referencing map.map at the same time.
        // Note that under no circumstances should map.map ever be modified.
        // Not for *this* revision, but for every revision.
        // The memory map must remain in its original state (even if its moved).
        let map_static: &MemoryMap = unsafe { &*(&raw const map.map) };

        map.dma_64.drain(&mut phys_alloc.mem_64, &map_static);
        map.dma_32.drain(&mut phys_alloc.mem_32, &map_static);
        map.dma_16.drain(&mut phys_alloc.mem_16, &map_static);
    }

    pub struct PreInitFrameAlloc {
        map: MemoryMap,
        dma_16: IterSegment,
        dma_32: IterSegment,
        dma_64: IterSegment,
    }

    impl PreInitFrameAlloc {
        pub fn new(map: MemoryMap) -> Self {
            Self {
                dma_16: IterSegment::new(MemRegion::Mem16),
                dma_32: IterSegment::new(MemRegion::Mem32),
                dma_64: IterSegment::new(MemRegion::Mem64),
                map,
            }
        }
    }

    unsafe impl x86_64::structures::paging::FrameAllocator<Size4KiB> for PreInitFrameAlloc {
        fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
            if let Some(addr) = self.dma_64.alloc(&self.map) {
                return Some(PhysFrame::from_start_address(addr).unwrap());
            }
            // Just try every list until it works
            if let Some(addr) = self.dma_32.alloc(&self.map) {
                return Some(PhysFrame::from_start_address(addr).unwrap());
            }
            if let Some(addr) = self.dma_16.alloc(&self.map) {
                return Some(PhysFrame::from_start_address(addr).unwrap());
            }

            None
        }
    }

    struct IterSegment {
        mem_region: MemRegion,
        current: Option<PhysAddr>,
        seg_num: usize,
        len: u64,
    }

    impl IterSegment {
        const fn new(mem_region: MemRegion) -> Self {
            Self {
                mem_region,
                current: None,
                seg_num: 0,
                len: 0, // Note that this has no value while `current` is not set
            }
        }

        fn alloc(&mut self, map: &MemoryMap) -> Option<PhysAddr> {
            if let Some(rc) = self.current.take() {
                self.len -= super::PAGE_SIZE as u64;
                if self.len == 0 {
                    return Some(rc);
                }
                self.current = Some(rc + super::PAGE_SIZE as u64);
                Some(rc)
            } else {
                self.advance(map);
                // Duplicate of above
                if let Some(rc) = self.current.take() {
                    self.len -= super::PAGE_SIZE as u64;
                    if self.len == 0 {
                        return Some(rc);
                    }
                    self.current = Some(rc + super::PAGE_SIZE as u64);
                    Some(rc)
                } else {
                    None
                }
            }
        }

        fn advance(&mut self, map: &MemoryMap) {
            // UNSAFE: No preconditions currently exist
            let iter = unsafe { map.iter() };
            for (i, region) in iter
                .enumerate()
                .skip(self.seg_num + 1)
                .filter(|(_, region)| region.ty == hatcher::boot_info::MemoryRegionType::Usable)
            {
                let start = region.phys_addr.max(self.mem_region.lower_limit());
                let end = (region.phys_addr + region.size).min(self.mem_region.upper_limit());

                if start < end {
                    self.current
                        .replace(PhysAddr::new(start))
                        .ok_or(())
                        .expect_err("Preemptively attempted to advance IterSegment");
                    self.len = end - start;
                    self.seg_num = i;
                    return;
                }
            }
        }

        fn pop(&mut self, map: &MemoryMap) -> Option<(PhysAddr, u64)> {
            match self.current.take() {
                Some(addr) => Some((addr, core::mem::take(&mut self.len))),
                None => {
                    self.advance(map);
                    let addr = self.current.take()?;
                    let len = core::mem::take(&mut self.len);
                    Some((addr, len))
                }
            }
        }

        fn drain(&mut self, region: &mut super::Region, map: &MemoryMap) {
            while let Some((mut addr, mut len)) = self.pop(map) {
                crate::serial_println!("---pop---");
                while let Some((base, len)) = Self::dissolve(&mut addr, &mut len) {
                    crate::serial_println!("free - base: {:x?}, len: {:x?}", base, len);
                    region.deallocate(base, len);
                }
            }
        }

        fn dissolve(addr: &mut PhysAddr, len: &mut u64) -> Option<(PhysAddr, u64)> {
            if *len == 0 {
                return None;
            }
            // Absolute maximum size. Exceeding this will result in unaligned block.
            // unwrap_or: It's OK we'll never have a block that large. If we do then its 2 ops instead of one.
            let mut tgt_size = 1u64
                .checked_shl(addr.as_u64().trailing_zeros())
                .unwrap_or(1 << 63);

            if tgt_size > *len {
                // Always smaller than `tgt_size`
                tgt_size = 1 << (u64::BITS - (len.leading_zeros() + 1));
            }
            let r = *addr;
            *addr += tgt_size;
            *len -= tgt_size;

            Some((r, tgt_size))
        }
    }
}

pub(crate) struct BuddyFrameAlloc {
    mem_16: Region,
    mem_32: Region,
    mem_64: Region,
}

impl BuddyFrameAlloc {
    pub(super) const fn new() -> Self {
        Self {
            mem_16: Region::new(),
            mem_32: Region::new(),
            mem_64: Region::new(),
        }
    }

    pub fn allocate(
        &mut self,
        layout: Layout,
        region: MemRegion,
    ) -> Result<PhysAddr, core::alloc::AllocError> {
        let tgt_size = layout.size().max(layout.align());

        match region {
            MemRegion::Mem16 => self.mem_16.allocate(tgt_size as u64),
            MemRegion::Mem32 => {
                let Ok(rc) = self.mem_32.allocate(tgt_size as u64) else {
                    return self.allocate(layout, MemRegion::Mem16);
                };
                Ok(rc)
            }
            MemRegion::Mem64 => {
                let Ok(rc) = self.mem_64.allocate(tgt_size as u64) else {
                    return if let Ok(r) = self.allocate(layout, MemRegion::Mem32) {
                        Ok(r)
                    } else {
                        MEM_MAP
                            .get()
                            .allocate_frame()
                            .map(|f: PhysFrame| f.start_address())
                            .ok_or(AllocError)
                    };
                };
                Ok(rc)
            }
        }
    }

    /// Frees the physical memory at `addr..addr+layout.size()`.
    ///
    /// # Safety
    ///
    /// See [core::alloc::Allocator::deallocate] for safety preconditions.
    pub unsafe fn dealloc(&mut self, addr: PhysAddr, layout: Layout) {
        let tgt_size = layout.size().max(layout.align());
        let region = MemRegion::from(addr);

        match region {
            MemRegion::Mem16 => self.mem_16.deallocate(addr, tgt_size as u64),
            MemRegion::Mem32 => self.mem_32.deallocate(addr, tgt_size as u64),
            MemRegion::Mem64 => self.mem_64.deallocate(addr, tgt_size as u64),
        }
    }
}

struct Region {
    backend: BackedConfig,
    high_order_alloc: crate::mem::high_order_alloc::HighOrderAlloc<u64, HIGH_ORDER_BLOCK_SIZE>,
    extend_recurse: core::sync::atomic::AtomicBool,
}

impl Region {
    const fn new() -> Self {
        Self {
            backend: BackedConfig::new(alloc::alloc::Global),
            high_order_alloc: crate::mem::high_order_alloc::HighOrderAlloc::new(),
            extend_recurse: core::sync::atomic::AtomicBool::new(false),
        }
    }

    fn use_high(size: u64) -> bool {
        size > BackedConfig::ORDER_MAX_SIZE as u64
    }

    /// Extends `self.backend` using memory from `self.high_order_alloc`.
    ///
    /// Because `self.backend` cant store the whole block, the higher half of the block's address
    /// will be returned which can be "freed" into `self.backend` after the original allocation is satisfied.
    /// The size of the returned block is [MAX_ORDER_SIZE].
    fn extend_from_high(&mut self) -> Result<memory_addresses::PhysAddr, ()> {
        // When running this function memory may be allocated from `self` to satisfy storing its own free list.
        // This checks if this function is recursing. If it is it fails and the caller should call self.extend instead.
        match self.extend_recurse.compare_exchange(
            false,
            true,
            atomic::Ordering::Acquire,
            atomic::Ordering::Relaxed,
        ) {
            Ok(_) => {}
            Err(_) => return Err(()),
        }
        let Some(t) = self.high_order_alloc.allocate(
            Layout::from_size_align(
                HIGH_ORDER_BLOCK_SIZE as usize,
                HIGH_ORDER_BLOCK_SIZE as usize,
            )
            .unwrap(),
        ) else {
            self.extend_recurse.store(false, atomic::Ordering::Release);
            return Err(());
        };
        let base = memory_addresses::PhysAddr::new(t.as_raw().0 as u64);
        let higher = base + MAX_ORDER_SIZE as u64;

        // We use base here because backend will allocate the lowest value blocks, meaning that the
        // start of free memory after the original allocation is satisfied will be contiguous with the higher block.
        self.backend
            .deallocate(HIGH_ORDER_BLOCK_SIZE as usize, base)
            .unwrap();
        self.extend_recurse.store(false, atomic::Ordering::Release);
        Ok(higher)
    }

    fn allocate(&mut self, size: u64) -> Result<PhysAddr, core::alloc::AllocError> {
        if size == 0 {
            log::debug!("Attempted to allocate 0 bytes of physical memory. Returning 0x0");
            return Ok(PhysAddr::zero());
        }

        let size = size.next_power_of_two();

        if Self::use_high(size) {
            let r = self
                .high_order_alloc
                .allocate(Layout::from_size_align(size as usize, size as usize).unwrap())
                .ok_or(core::alloc::AllocError)?;
            return Ok(PhysAddr::new(r.as_raw().0));
        }

        let ret = match self.backend.allocate(size as usize) {
            Ok(a) => a,
            Err(_) => {
                let hold = self
                    .extend_from_high()
                    .map_err(|_| core::alloc::AllocError)?;

                let rc = self.backend.allocate(size as usize)?;
                // This can never return the buddy.
                // Part of the buddy is `rc`, this can be guaranteed because
                // self.high_order_alloc **only** deals is BS aligned blocks.
                self.backend.deallocate(MAX_ORDER_SIZE, hold).unwrap();
                rc
            }
        };

        // unwrap: Inserting `0` into self is a logical error.
        Ok(ret.into())
    }

    fn deallocate(&mut self, base: PhysAddr, size: u64) {
        if size == 0 {
            return;
        }
        // `backend` can only return powers of two, this helps sanitize it.
        let size = size.next_power_of_two();

        if Self::use_high(size) {
            self.high_order_alloc
                .free(FreeMem::new(base.as_u64(), size).unwrap())
                .unwrap();
        } else {
            match self
                .backend
                .deallocate(size.try_into().unwrap(), base.into())
            {
                Ok(()) => return,
                Err(addr) => self
                    .high_order_alloc
                    .free(FreeMem::new(addr.as_u64(), HIGH_ORDER_BLOCK_SIZE as u64).unwrap())
                    .unwrap(),
            }
        }
    }
}

macro_rules! impl_frame_alloc {
    ($size:ty,$size_num:expr) => {
        unsafe impl x86_64::structures::paging::FrameAllocator<$size> for BuddyFrameAlloc {
            fn allocate_frame(&mut self) -> Option<PhysFrame<$size>> {
                let rc = self
                    .allocate(
                        Layout::from_size_align($size_num, $size_num).unwrap(),
                        MemRegion::Mem64,
                    )
                    .ok();
                rc.map(|addr| PhysFrame::from_start_address(addr).unwrap())
            }
        }

        impl x86_64::structures::paging::FrameDeallocator<$size> for BuddyFrameAlloc {
            unsafe fn deallocate_frame(&mut self, frame: PhysFrame<$size>) {
                // SAFETY: Upheld by caller
                unsafe {
                    self.dealloc(
                        frame.start_address(),
                        Layout::from_size_align($size_num, $size_num).unwrap(),
                    )
                }
            }
        }
    };
}

impl_frame_alloc!(Size4KiB, PAGE_SIZE);
impl_frame_alloc!(Size2MiB, suffix::bin!(2M));
impl_frame_alloc!(Size1GiB, suffix::bin!(1G));

impl super::MemRegion {
    const fn upper_limit(&self) -> u64 {
        match self {
            MemRegion::Mem16 => 0xffff,
            MemRegion::Mem32 => 0xffffffff,
            MemRegion::Mem64 => 0xffffffffffffffff,
        }
    }

    const fn lower_limit(&self) -> u64 {
        match self {
            MemRegion::Mem16 => 0,
            MemRegion::Mem32 => 0x10000,
            MemRegion::Mem64 => 0x100000000,
        }
    }
}
