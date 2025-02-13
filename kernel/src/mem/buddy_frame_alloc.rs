use super::MemRegion;
use super::PAGE_SIZE;
use crate::util::mutex::ReentrantMutex;
use x86_64::PhysAddr;

// todo: handle 2M and 1G blocks separately from the allocator (lower order too)

const ORDERS: usize = 9;
const ORDER_MAX_SIZE: usize = PAGE_SIZE << (ORDERS - 1);
/// ratio of [ORDER_MAX_SIZE] to [HIGH_ORDER_BLOCK_SIZE]
const HIGH_ORDER_BLOCK_RATIO: u32 = 2;
const HIGH_ORDER_BLOCK_SIZE: u32 = (ORDER_MAX_SIZE as u32) * HIGH_ORDER_BLOCK_RATIO;

static MEM_MAP: crate::util::KernelStatic<PreInitFrameAlloc> = crate::util::KernelStatic::new();

pub(super) unsafe fn init_mem_map(regions: libboot::boot_info::MemoryMap) {
    MEM_MAP.init(PreInitFrameAlloc::new(regions));
}

struct PreInitFrameAlloc {
    list: libboot::boot_info::MemoryMap,
    mem_16_n: Option<usize>,
    mem_32_n: Option<usize>,
    mem_64_n: Option<usize>,
}

impl PreInitFrameAlloc {
    /// Creates a new PreInitFrameAlloc from the given memory regions
    ///
    /// # Safety
    ///
    /// This fn is unsafe because the caller must ensure that `regions` correctly describes physical
    /// memory
    unsafe fn new(regions: libboot::boot_info::MemoryMap) -> Self {
        let mut mem_16_n = None;
        let mut mem_32_n = None;
        let mut mem_64_n = None;

        for i in regions.iter() {
            crate::serial_println!("{:x?}", i);
        }
        macro_rules! usable_regions {
            ($regions:ident) => {
                $regions
                    .iter()
                    .filter(|p| p.ty == libboot::boot_info::MemoryRegionType::Usable)
            };
        }

        for i in usable_regions!(regions) {
            if let Some(n) = MemRegion::Mem16
                .first_in_region(i.phys_addr as usize, i.size as usize + i.phys_addr as usize)
            {
                mem_16_n = Some(n);
                break;
            }
        }

        for i in usable_regions!(regions) {
            if let Some(n) = MemRegion::Mem32
                .first_in_region(i.phys_addr as usize, i.size as usize + i.phys_addr as usize)
            {
                mem_32_n = Some(n);
                break;
            }
        }

        for i in usable_regions!(regions) {
            if let Some(n) = MemRegion::Mem64
                .first_in_region(i.phys_addr as usize, i.size as usize + i.phys_addr as usize)
            {
                mem_64_n = Some(n);
                break;
            }
        }

        Self {
            list: regions,
            mem_16_n,
            mem_32_n,
            mem_64_n,
        }
    }

    #[inline]
    fn alloc(&mut self, region: MemRegion) -> Option<usize> {
        // This uses take because if a new region is found the region_n **must** be updated
        let ret = match region {
            MemRegion::Mem16 => self.mem_16_n.take()?,

            MemRegion::Mem32 => {
                if let Some(l) = self.mem_32_n.take() {
                    l
                } else {
                    return self.alloc(MemRegion::Mem16);
                }
            }

            MemRegion::Mem64 => {
                if let Some(l) = self.mem_64_n.take() {
                    l
                } else {
                    return self.alloc(MemRegion::Mem32);
                }
            }
        };

        let mut iter = self
            .list
            .iter()
            .filter(|p| p.ty == libboot::boot_info::MemoryRegionType::Usable);

        // Loops over memory region list to locate the next region.
        'base: while let Some(i) = iter.next() {
            if (ret >= i.phys_addr as usize) && (ret < i.size as usize + i.phys_addr as usize) {
                // ret is found, now figure out next addr
                if let Some(ptr) =
                    region.first_in_region(ret + PAGE_SIZE, i.size as usize + i.phys_addr as usize)
                {
                    // next is in same memory region
                    match region {
                        MemRegion::Mem16 => self.mem_16_n = Some(ptr),
                        MemRegion::Mem32 => self.mem_32_n = Some(ptr),
                        MemRegion::Mem64 => self.mem_64_n = Some(ptr),
                    }
                    break 'base;
                } else {
                    // locate next memory region
                    while let Some(i) = iter.next() {
                        // Will only return Some() if this memory region is within the correct dma region
                        if let Some(next) = region
                            .first_in_region(i.phys_addr as usize, (i.size + i.phys_addr) as usize)
                        {
                            match region {
                                MemRegion::Mem16 => self.mem_16_n = Some(next),
                                MemRegion::Mem32 => self.mem_32_n = Some(next),
                                MemRegion::Mem64 => self.mem_64_n = Some(next),
                            }
                            break 'base;
                        }
                    }
                }
            }
        }
        Some(ret)
    }

    fn peek(&self, region: MemRegion) -> Option<usize> {
        let ret = match region {
            MemRegion::Mem16 => self.mem_16_n?,

            MemRegion::Mem32 => {
                if let Some(l) = self.mem_32_n {
                    l
                } else {
                    return self.peek(MemRegion::Mem16);
                }
            }

            MemRegion::Mem64 => {
                if let Some(l) = self.mem_64_n {
                    l
                } else {
                    return self.peek(MemRegion::Mem32);
                }
            }
        };
        Some(ret)
    }
}

/// Properly initializes [super::SYS_FRAME_ALLOC] this cannot be done too early to because it relies
/// on [alloc::alloc::Global]. Drains the unused memory given to [init_mem_map] into
/// `super::SYS_FRAME_ALLOC`. This fn should be called immediately after initializing the Global heap.
///
/// # Panics
///
/// This fn will panic if called more than once
#[cold]
pub fn drain_map() {
    /*
        This works by getting the first free frame then locating it in the map. Once the first frame
        is located all following regions within the map are drained into the BuddyFrameAlloc until
        either the end of the DMA region is found or the end of the map
    */

    //let f_alloc = super::SYS_FRAME_ALLOCATOR.alloc.lock();
    //for i in f_alloc.mem_16.free_list {}

    assert!(super::allocator::COMBINED_ALLOCATOR
        .lock()
        .phys_alloc()
        .alloc
        .lock()
        .is_fully_init
        .is_none());

    // Must be in smallest -> largest order or alloc will try to return the wrong region
    drain_map_inner(MemRegion::Mem16);
    super::allocator::COMBINED_ALLOCATOR
        .lock()
        .phys_alloc()
        .alloc
        .lock()
        .is_fully_init = Some(MemRegion::Mem16);
    drain_map_inner(MemRegion::Mem32);
    super::allocator::COMBINED_ALLOCATOR
        .lock()
        .phys_alloc()
        .alloc
        .lock()
        .is_fully_init = Some(MemRegion::Mem32);
    drain_map_inner(MemRegion::Mem64);
    super::allocator::COMBINED_ALLOCATOR
        .lock()
        .phys_alloc()
        .alloc
        .lock()
        .is_fully_init = Some(MemRegion::Mem64);
}

/// Inner fn for `drain_map()` drains all of `MEM_MAP`'s region into `super::SYS_FRAME_ALLOC`
fn drain_map_inner(region: MemRegion) {
    // Iterates over the mem map until allocator can find no allowed frames
    '_list: loop {
        let mut map = MEM_MAP.get();

        let base = if let Some(base) = map.alloc(region) {
            base
        } else {
            break;
        };

        let mut len = PAGE_SIZE;

        // Checks for contiguous memory. This will traverse contiguous end to end blocks
        'block: while let Some(new) = map.peek(region) {
            if new == len + base {
                len += PAGE_SIZE;
                map.alloc(region);
            } else {
                break 'block;
            }
        }

        drop(map);

        #[cfg(feature = "alloc-debug-serial")]
        crate::serial_println!(
            "Draining region {:?} starting: {:#x}, len: {:#x} end {:#x}",
            region,
            base,
            len,
            len + base
        );

        let b = super::allocator::COMBINED_ALLOCATOR.lock();
        #[cfg(not(feature = "alloc-debug-serial"))]
        let f_alloc = b.phys_alloc().get().frame_alloc.alloc.lock();

        #[cfg(feature = "alloc-debug-serial")]
        let mut f_alloc = b.phys_alloc().get().frame_alloc.alloc.lock();

        #[cfg(feature = "alloc-debug-serial")]
        let cache = {
            let mut cache = [0usize; ORDERS];
            for (i, l) in region.list(&mut *f_alloc).free_list.iter().enumerate() {
                cache[i] = l.len()
            }
            cache
        };

        drop(f_alloc);

        // SAFETY: region is given by frame allocator and is unused
        unsafe {
            super::allocator::COMBINED_ALLOCATOR
                .lock()
                .phys_alloc()
                .dealloc(base, len)
        }

        #[cfg(feature = "alloc-debug-serial")]
        {
            let mut f_alloc = b.phys_alloc().get().frame_alloc.alloc.lock();
            let mut new_len = [0usize; ORDERS];
            for (i, l) in region.list(&mut *f_alloc).free_list.iter().enumerate() {
                new_len[i] = l.len()
            }
            crate::serial_println!("new: {:?}", new_len);
            let mut diff = [0isize; ORDERS];

            for (i, (c, n)) in core::iter::zip(cache, new_len).enumerate() {
                diff[i] = (n - c) as isize
            }

            crate::serial_println!("diff {:?}", diff);

            let mut sum = 0;
            for i in 0..ORDERS {
                let mul = (super::PAGE_SIZE << i) as isize;
                let n = diff[i] * mul;
                sum = sum + n;
            }
        }
    }
}

pub struct BuddyFrameAlloc {
    alloc: ReentrantMutex<FrameAllocInner>,
}

struct FrameAllocInner {
    is_fully_init: Option<MemRegion>,
    mem_16: DmaRegion,
    mem_32: DmaRegion,
    mem_64: DmaRegion,
}

struct DmaRegion {
    free_list: [alloc::collections::LinkedList<usize>; ORDERS],
    high_order: super::high_order_alloc::HighOrderAlloc<u64, HIGH_ORDER_BLOCK_SIZE>,
}

impl DmaRegion {
    /// Retrieves a block of size `order`, splitting and extending the heap as necessary
    fn fetch(&mut self, order: usize) -> Option<usize> {
        if let Some(block) = self.free_list[order].pop_front() {
            Some(block)
        } else {
            // attempts to allocate more memory from the high order allocator
            if let Err(()) = self.split(order) {
                // I'd prefer this to panic at compile time
                let l = core::alloc::Layout::from_size_align(
                    HIGH_ORDER_BLOCK_SIZE as usize,
                    HIGH_ORDER_BLOCK_SIZE as usize,
                )
                .unwrap();
                let (addr, _) = self.high_order.allocate(l)?.as_raw();
                for i in 0..(HIGH_ORDER_BLOCK_RATIO as u64) {
                    let addr = addr + (ORDER_MAX_SIZE * i as usize) as u64;
                    // the new block should be inserted without merging with a buddy.
                    self.free_list[ORDERS - 1].push_front(addr as usize);
                }

                self.split(order).ok()?;
            }

            let ret = self.free_list[order].pop_front().unwrap(); // failure handled already
            return Some(ret);
        }
    }

    /// Breaks unused blocks to create at least one free block at the target order.
    fn split(&mut self, order: usize) -> Result<(), ()> {
        for i in order..ORDERS {
            if let Some(block) = self.free_list[i].pop_front() {
                for j in (order..i).rev() {
                    let buddy = block ^ Self::block_size(j);
                    self.free_list[j].push_front(buddy);
                }

                self.free_list[order].push_front(block);
                return Ok(());
            }
        }

        Err(())
    }

    /// Calculates the size of block form its order
    const fn block_size(order: usize) -> usize {
        assert!(order <= ORDERS, "Order above allowed scope");
        PAGE_SIZE << order
    }

    /// Returns the order that should be used to store the given layout. Will choose a larger block
    /// for larger alignments.
    #[inline]
    fn order_from_size(size: usize) -> Option<usize> {
        let mut use_order = None;

        for i in 0..ORDERS {
            let order_size = PAGE_SIZE << i;
            if size <= order_size {
                use_order = Some(i);
                break;
            }
        }

        use_order
    }

    /// Returns the order for size only if size exactly matches the order size
    #[inline]
    fn order_from_exact_size(size: usize) -> Option<usize> {
        for i in 0..ORDERS {
            let order_size = PAGE_SIZE << i;
            if size == order_size {
                return Some(i);
            }
        }
        None
    }

    /// Rejoins the described block into the free list, joining as many buddies as possible.
    fn rejoin(&mut self, addr: usize, order: usize) {
        let mut use_ord = ORDERS - 1;
        for i in order..ORDERS {
            let buddy = addr ^ Self::block_size(i);

            // just removing the buddy is fine enough

            let mut filter = self.free_list[i].extract_if(|n| *n == buddy);

            if let None = filter.next() {
                // still calls Box::new
                drop(filter);
                use_ord = i;
                break;
            } else if i == ORDERS - 1 {
                // I could make this locate any adjacent memory regions and free those but this is just convenient

                // gets the address of the lower buddy regardless of whether this is the higher one or not.
                let ba = addr as u64 & !(HIGH_ORDER_BLOCK_SIZE - 1) as u64;

                // Will never return None
                let f = super::high_order_alloc::FreeMem::new(ba, HIGH_ORDER_BLOCK_SIZE as u64)
                    .unwrap();
                self.high_order
                    .free(f)
                    .expect("Failed to move memory into HighOrderAlloc");
            }
        }
        self.free_list[use_ord].push_front(addr)
    }
}

impl super::MemRegion {
    fn list<'a>(&self, alloc: &'a mut FrameAllocInner) -> &'a mut DmaRegion {
        match self {
            MemRegion::Mem16 => &mut alloc.mem_16,
            MemRegion::Mem32 => &mut alloc.mem_32,
            MemRegion::Mem64 => &mut alloc.mem_64,
        }
    }

    const fn upper_limit(&self) -> usize {
        match self {
            MemRegion::Mem16 => 0xffff,
            MemRegion::Mem32 => 0xffffffff,
            MemRegion::Mem64 => 0xffffffffffffffff,
        }
    }

    const fn lower_limit(&self) -> usize {
        match self {
            MemRegion::Mem16 => 0,
            MemRegion::Mem32 => 0x10000,
            MemRegion::Mem64 => 0x100000000,
        }
    }

    /// Returns the lowest address in this region of the specified addresses.
    /// If the arguments cross the lower region boundary that address will be returned.
    /// If the arguments do not the requested region this will return `None`
    ///
    /// # Panics
    ///
    /// This fn will panic if start > end
    #[inline]
    const fn first_in_region(&self, start: usize, end: usize) -> Option<usize> {
        if start == end {
            return None;
        }
        assert!(start < end); // Dont do this
        if start > self.upper_limit() {
            return None;
        } else if end < self.lower_limit() {
            return None;
        }

        return if start >= self.lower_limit() {
            Some(start)
        } else {
            Some(self.lower_limit())
        };
    }
}

impl FrameAllocInner {
    /// Attempt to allocate `size` bytes from  physical memory size will be aligned to the next
    /// highest power of two
    ///
    /// # Panics
    ///
    /// `size` must not greater than [ORDER_MAX_SIZE]. `size` must be an exact size for one of the internal order sizes.
    // todo: clear the second panic source.
    // the second panic source exists to prevent memory leaks.
    // if size is 12K find one order 2(16K) block and break it, allocate the first block (8k) break
    // the second and allocate the first block(4k) retain the fourth order 0 block
    // |b      | b: break,
    // |u  |b  | u: use,
    //     |u|k| k: keep,

    fn allocate(&mut self, size: usize, region: MemRegion) -> Option<usize> {
        assert!(
            size <= ORDER_MAX_SIZE,
            "Attempted to allocate {} bytes use `BuddyFrameAlloc::alloc_huge` instead",
            size
        );
        assert_eq!(size & (PAGE_SIZE - 1), 0);

        match region {
            MemRegion::Mem16 => self.mem_16.fetch(
                DmaRegion::order_from_exact_size(size).expect("Failed to get exact order for size"),
            ),
            MemRegion::Mem32 => self
                .mem_32
                .fetch(
                    DmaRegion::order_from_exact_size(size)
                        .expect("Failed to get exact order for size"),
                )
                .or_else(|| self.allocate(size, MemRegion::Mem16)),
            MemRegion::Mem64 => self
                .mem_64
                .fetch(
                    DmaRegion::order_from_exact_size(size)
                        .expect("Failed to get exact order for size"),
                )
                .or_else(|| self.allocate(size, MemRegion::Mem32)),
        }
    }

    /// Deallocates the given region will fewer restrictions but a higher complexity
    ///
    /// # Panics
    ///
    /// `ptr` and `size` must be aligned to 4K
    ///
    /// # Safety
    ///
    /// The caller must ensure that the given region is unused and correctly describes the region
    /// to be deallocated
    unsafe fn dealloc_exact(&mut self, ptr: usize, size: usize) {
        assert_eq!(ptr & (PAGE_SIZE - 1), 0);
        assert_eq!(size & (PAGE_SIZE - 1), 0);

        let mut ptr = self.align_region(DmaDecompose::new(ptr, size));

        // drain
        loop {
            for order in (0..ORDERS).rev() {
                let bs = PAGE_SIZE << order;
                while ptr.len >= bs {
                    ptr.region.list(self).rejoin(ptr.ptr, order);

                    if let Some(new) = ptr.advance(bs) {
                        ptr = new;
                    } else {
                        // None signals that ptr is fully depleted
                        return;
                    }
                }
            }

            // do while
            if let Some(n) = ptr.next_region() {
                ptr = n
            } else {
                break;
            }
        }
    }

    /// Aligns `region` to the highest alignment it can, potentially reducing `region.size` to 0.
    /// This is done by deallocating regions using [Self::dealloc]
    ///
    /// # Safety
    ///
    /// This fn calls [Self::dealloc] see source for details
    unsafe fn align_region(&mut self, region: DmaDecompose) -> DmaDecompose {
        let mut start = region.ptr;
        let mut len = region.len;
        if let Some(n) = region.remain_ptr {
            len += n;
        }

        for order in 0..ORDERS - 1 {
            // skip last order because its always aligned
            let addr = start;
            let r_size = PAGE_SIZE << order; // region size

            // if region size is greater than actual size.
            if r_size > len {
                // The idea is this fn will provide a walk up for the region and another fn provides
                // a walk down. if region size is greater than actual size then a walk down would be
                // aligned already
                break;
            }

            if r_size & addr != 0 {
                self.dealloc(addr, r_size).unwrap(); // returning Err(()) is considered a bug
                start += r_size;
                len -= r_size;
            }
        }
        DmaDecompose::new(start, len)
    }

    /// Deallocates the given memory returns the status of the deallocation.
    ///
    /// Try [Self::dealloc_exact] on `Err(())`
    ///
    /// # Panics
    ///
    /// `size` must be > [PAGE_SIZE], and < [MAX_ORDER_SIZE] and power of 2.
    /// `ptr` must be aligned to `size`
    /// The given region must not cross DMA regions
    ///
    /// # Safety
    ///
    /// The caller must ensure that the given region is unused and correctly describes the region
    /// to be deallocated
    unsafe fn dealloc(&mut self, ptr: usize, size: usize) -> Result<(), ()> {
        if !size.is_power_of_two() {
            return Err(());
        }
        if ptr & (size - 1) != 0 {
            return Err(());
        }
        let region = DmaDecompose::new(ptr, size);
        if region.is_multiple_regions() {
            return Err(());
        }

        let list = match region.region {
            MemRegion::Mem16 => &mut self.mem_16,
            MemRegion::Mem32 => &mut self.mem_32,
            MemRegion::Mem64 => &mut self.mem_64,
        };

        list.rejoin(ptr, DmaRegion::order_from_size(size).ok_or(())?); // shouldn't panic might do so anyway

        Ok(())
    }
}
/// A struct for handling calculations relating to memory regions
#[derive(Copy, Clone)]
struct DmaDecompose {
    region: MemRegion,
    ptr: usize,
    len: usize,
    remain_ptr: Option<usize>,
    remain_len: Option<usize>,
}

impl DmaDecompose {
    const DMA16_MAX: usize = u16::MAX as usize;
    const DMA32_MAX: usize = u32::MAX as usize;

    fn new(ptr: usize, len: usize) -> Self {
        let last = ptr + len - 1;
        let use_type;
        let use_len;
        let mut remain_ptr = None;
        let mut remain_len = None;

        if let None = ptr.checked_sub(Self::DMA16_MAX) {
            use_type = MemRegion::Mem16;
            if Self::DMA16_MAX > last {
                use_len = len;
            } else {
                remain_ptr = Some(Self::DMA16_MAX + 1);
                remain_len = Some((Self::DMA16_MAX + 1) - (ptr + len));
                use_len = remain_len.unwrap() - len
            }
        } else if let None = ptr.checked_sub(Self::DMA32_MAX) {
            use_type = MemRegion::Mem32;
            if Self::DMA32_MAX > last {
                use_len = len;
            } else {
                remain_ptr = Some(Self::DMA32_MAX + 1);
                remain_len = Some((Self::DMA32_MAX + 1) - (ptr + len));
                use_len = remain_len.unwrap() - len
            }
        } else {
            use_type = MemRegion::Mem64;
            use_len = len;
        }

        Self {
            region: use_type,
            ptr,
            len: use_len,
            remain_ptr,
            remain_len,
        }
    }

    fn next_region(self) -> Option<Self> {
        Some(Self::new(self.remain_ptr?, self.remain_ptr?))
    }

    /// Returns a new Self with the first `bytes` removed, advancing the ptr and decreasing the len.
    fn advance(self, bytes: usize) -> Option<Self> {
        let true_len = self.len.checked_add(self.remain_len.unwrap_or(0))?;
        if true_len == 0 {
            return None;
        }
        if bytes > true_len {
            return None;
        }

        Some(Self::new(self.ptr + bytes, true_len - bytes))
    }
    fn is_multiple_regions(&self) -> bool {
        return if let Some(_) = self.remain_len {
            true
        } else {
            false
        };
    }
}

impl BuddyFrameAlloc {
    pub const fn new() -> Self {
        Self {
            alloc: ReentrantMutex::new(FrameAllocInner {
                is_fully_init: None,

                mem_16: DmaRegion {
                    free_list: [const { alloc::collections::LinkedList::new() }; ORDERS],
                    high_order: super::high_order_alloc::HighOrderAlloc::new(),
                },

                mem_32: DmaRegion {
                    free_list: [const { alloc::collections::LinkedList::new() }; ORDERS],
                    high_order: super::high_order_alloc::HighOrderAlloc::new(),
                },

                mem_64: DmaRegion {
                    free_list: [const { alloc::collections::LinkedList::new() }; ORDERS],
                    high_order: super::high_order_alloc::HighOrderAlloc::new(),
                },
            }),
        }
    }

    /// Allocates `size` bytes of physical memory at the returned physical address.
    ///
    /// If `size` is lower than [ORDER_MAX_SIZE] then allocations will be aligned to he next highest
    /// power of two. If `size` is above [ORDER_MAX_SIZE] allocations will be aligned to [ORDER_MAX_SIZE]
    pub fn allocate(&self, layout: core::alloc::Layout, mut region: MemRegion) -> Option<usize> {
        let mut alloc = self.alloc.lock();
        let limit = layout.size().max(layout.align());

        match alloc.is_fully_init {
            Some(MemRegion::Mem64) => {}
            Some(r) => {
                assert!(layout.align() <= 0x1000);
                assert_eq!(layout.size(), 0x1000);
                if let Some(ret) = MEM_MAP.get().alloc(region) {
                    return Some(ret);
                } else if r == MemRegion::Mem16 {
                    // This is a hack fix to prevent my single frame of free mem16 memory from being
                    // used while initializing the allocator
                    // This alloc() call hasn't yet modified the inner state machine so this shouldn't do anything bad
                    if let Some(r) = alloc.allocate(limit, MemRegion::Mem32) {
                        return Some(r);
                    } else if r < region {
                        region = r
                    }
                }
            }
            None => {
                assert!(layout.align() <= 0x1000);
                assert_eq!(layout.size(), 0x1000);
                return MEM_MAP.get().alloc(region);
            }
        }

        if limit > ORDER_MAX_SIZE {
            self.alloc_huge(layout, region)
        } else {
            alloc.allocate(limit, region)
        }
    }

    /// Attempts to allocate a region larger than `ORDER_MAX_SIZE`. This fn shouldn't be used
    /// normally it is required for DMA allocations larger than 2Mib
    pub fn alloc_huge(&self, layout: core::alloc::Layout, mut region: MemRegion) -> Option<usize> {
        let limit = layout.size().max(layout.align());
        assert!(
            limit > ORDER_MAX_SIZE,
            "tried to allocate_huge where size is <2Mib"
        );

        let mut alloc = self.alloc.lock();
        if alloc.is_fully_init.is_none() {
            return None;
        }
        let list = {
            loop {
                let chk_list = region.list(&mut *alloc).free_list.last_mut().unwrap(); // never none
                if chk_list.len() == 0 {
                    match region {
                        MemRegion::Mem16 => return None,
                        MemRegion::Mem32 => region = MemRegion::Mem16,
                        MemRegion::Mem64 => region = MemRegion::Mem32,
                    }
                } else {
                    break chk_list;
                }
            }
        };
        let mut arr = alloc::vec::Vec::with_capacity(list.len());

        // ensure that list is not empty all
        list.front()?;

        // fill and sort arr
        for i in list.iter() {
            arr.push(*i)
        }
        arr.sort();

        // find available region
        let req = layout.size().div_ceil(ORDER_MAX_SIZE);
        let mut found_count = 0;
        let align_mask = layout.align() - 1;
        let mut found = None;

        for i in &arr {
            match found {
                None => {
                    if i & align_mask == 0 {
                        found = Some(*i);
                        found_count = 1;
                    }
                }

                // if n == next expected block
                Some(n) if n == i - (ORDER_MAX_SIZE * found_count) => {
                    found_count += 1;
                    if req == found_count {
                        break;
                    }
                }

                _ => {
                    found = None;
                    found_count = 0;
                }
            }
        }

        for (i, n) in list.iter_mut().enumerate() {
            *n = arr[i];
        }

        let ret = found?;

        let mut cursor = list.cursor_front_mut();
        // drain used blocks from alloc
        while ret != *cursor.current().unwrap() {
            cursor.move_next()
        }

        // deallocations should not be made until writeback is completed

        let mut hold_list = alloc::collections::LinkedList::new();

        for _ in 0..req {
            hold_list.append(&mut cursor.remove_current_as_list()?);
        }
        Some(ret)
    }

    /// Deallocates the physical address at `ptr` for `len` bytes
    ///
    /// # Panics
    ///
    /// This fn will panic if `ptr` is not aligned to [PAGE_SIZE]
    ///
    /// # Safety
    ///
    /// The caller must ensure that the given region is unused and correctly describes the region
    /// to be deallocated
    pub unsafe fn dealloc(&self, ptr: usize, len: usize) {
        assert_eq!(
            ptr & (PAGE_SIZE - 1),
            0,
            "Physical deallocation not page aligned"
        );

        let mut alloc = self.alloc.lock();

        alloc
            .dealloc(ptr, len)
            .unwrap_or_else(|()| alloc.dealloc_exact(ptr, len));
    }

    /// Creates a FrameAllocRef for using with the [FrameAllocator] trait
    /// FrameAllocator cannot be implemented on BuddyFrameAlloc because its methods take a &mut self
    pub fn get(&self) -> FrameAllocRef {
        FrameAllocRef { frame_alloc: &self }
    }
}

// FrameAllocator uses &mut but i need it to be &
pub struct FrameAllocRef<'a> {
    frame_alloc: &'a BuddyFrameAlloc,
}

use x86_64::structures::paging::{
    FrameAllocator, FrameDeallocator, PhysFrame, Size1GiB, Size2MiB, Size4KiB,
};

unsafe impl<'a> FrameAllocator<Size4KiB> for FrameAllocRef<'a> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let addr = self.frame_alloc.allocate(
            core::alloc::Layout::from_size_align(0x1000, 0x1000).unwrap(),
            MemRegion::Mem64,
        )?;

        Some(PhysFrame::from_start_address(PhysAddr::new(addr as u64)).unwrap())
    }
}

impl<'a> FrameDeallocator<Size4KiB> for FrameAllocRef<'a> {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        self.frame_alloc
            .alloc
            .lock()
            .dealloc(frame.start_address().as_u64() as usize, 0x1000)
            .unwrap(); // shouldn't panic
    }
}

unsafe impl<'a> FrameAllocator<Size2MiB> for FrameAllocRef<'a> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size2MiB>> {
        let addr = self.frame_alloc.allocate(
            core::alloc::Layout::from_size_align(0x200000, 0x200000).unwrap(),
            MemRegion::Mem64,
        )?;

        Some(PhysFrame::from_start_address(PhysAddr::new(addr as u64)).unwrap())
    }
}

impl<'a> FrameDeallocator<Size2MiB> for FrameAllocRef<'a> {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size2MiB>) {
        self.frame_alloc
            .alloc
            .lock()
            .dealloc(frame.start_address().as_u64() as usize, 0x200000)
            .unwrap(); // shouldn't panic
    }
}

unsafe impl<'a> FrameAllocator<Size1GiB> for FrameAllocRef<'a> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size1GiB>> {
        let addr = self.frame_alloc.allocate(
            core::alloc::Layout::from_size_align(0x40000000, 0x40000000).unwrap(),
            MemRegion::Mem64,
        )?;

        Some(PhysFrame::from_start_address(PhysAddr::new(addr as u64)).unwrap())
    }
}

impl<'a> FrameDeallocator<Size1GiB> for FrameAllocRef<'a> {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size1GiB>) {
        self.frame_alloc
            .alloc
            .lock()
            .dealloc_exact(frame.start_address().as_u64() as usize, 0x40000000);
    }
}
