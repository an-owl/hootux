use x86_64::{
    structures::paging::{
        frame::PhysFrameRangeInclusive, page::PageRangeInclusive, FrameAllocator, Mapper,
        OffsetPageTable, Page, PageSize, PageTable, PageTableFlags, PhysFrame, Size1GiB, Size2MiB,
        Size4KiB,
    },
    PhysAddr, VirtAddr,
};
use crate::mem::allocator::{buddy_alloc, combined_allocator, fixed_size_block};

// offset 0-11
// l1 12-2
// l2 21-29
// l3 30-38
// l4 39-47
// quick maffs

pub mod allocator;
pub mod buddy_frame_alloc;
mod high_order_alloc;
pub mod mem_map;
pub(self) mod offset_page_table;
pub mod thread_local_storage;
pub mod tlb;
pub mod write_combining;
pub mod virt_fixup;
mod frame_attribute_table;

pub const PAGE_SIZE: usize = 4096;


/// Run PreInitialization for SYS_FRAME_ALLOC
pub unsafe fn set_sys_frame_alloc(mem_map: libboot::boot_info::MemoryMap) {
    buddy_frame_alloc::init_mem_map(mem_map)
}
/// This is the page table tree for the higher half kernel is shared by all CPU's. It should be used
/// in the higher half of all user mode programs too.
pub(crate) static SYS_MAPPER: SysMapper = SysMapper{};

// TODO: remove in favour of re-working memory management.
pub(crate) struct SysMapper{}

impl SysMapper {
    pub fn get(&self) -> MapperWorkaround {
        MapperWorkaround { inner: allocator::COMBINED_ALLOCATOR.lock() }
    }
}
pub (crate) struct MapperWorkaround {
    inner: crate::util::mutex::ReentrantMutexGuard<'static, combined_allocator::DualHeap<buddy_alloc::BuddyHeap, fixed_size_block::NewFixedBlockAllocator>>,
}

impl core::ops::Deref for MapperWorkaround {
    type Target = offset_page_table::OffsetPageTable;
    fn deref(&self) -> &Self::Target {
        self.inner.mapper()
    }
}

impl core::ops::DerefMut for MapperWorkaround {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.mapper_mut()
    }
}

#[deprecated]
pub unsafe fn set_sys_mem_tree_no_cr3(new_mapper: offset_page_table::OffsetPageTable) {
    allocator::COMBINED_ALLOCATOR.lock().cfg_mapper(new_mapper);
}

/// Dummy frame allocator that amy be used with PageTableTree because PageTableTree will
/// never call the passed &impl FrameAllocator
pub struct DummyFrameAlloc;

unsafe impl FrameAllocator<Size4KiB> for DummyFrameAlloc {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        allocator::COMBINED_ALLOCATOR.lock().phys_alloc().get().allocate_frame()
    }
}
unsafe impl FrameAllocator<Size2MiB> for DummyFrameAlloc {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size2MiB>> {
        unimplemented!()
    }
}
unsafe impl FrameAllocator<Size1GiB> for DummyFrameAlloc {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size1GiB>> {
        unimplemented!()
    }
}

/// Returns a mutable reference to the active level 4 table.
///
/// This function is unsafe because the caller must guarantee that the
/// complete physical memory is mapped to virtual memory at the passed
/// `physical_memory_offset`. Also, this function must be only called once
/// to avoid aliasing `&mut` references (which is undefined behavior).
unsafe fn active_l4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    use x86_64::registers::control::Cr3;
    let (l4, _) = Cr3::read();

    let phys = l4.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    &mut *page_table_ptr
}

/// Initialize a new OffsetPageTable.
///
/// This function is unsafe because the caller must guarantee that the
/// complete physical memory is mapped to virtual memory at the passed
/// `physical_memory_offset`. Also, this function must be only called once
/// to avoid aliasing `&mut` references (which is undefined behavior).
pub unsafe fn init(offset: VirtAddr) -> offset_page_table::OffsetPageTable {
    offset_page_table::OffsetPageTable::new(offset)
}

/// This is here to break safety and should only be used under very
/// specific circumstances. Usage of this struct should be avoided
/// unless absolutely necessary.
///
/// Stores
#[allow(dead_code)]
pub(crate) struct VeryUnsafeFrameAllocator {
    addr: Option<PhysAddr>,
    virt_base: Option<VirtAddr>,
    advanced: usize,
}

#[allow(dead_code)]
impl VeryUnsafeFrameAllocator {
    /// Creates an instance of VeryUnsafeFrameAllocator
    ///
    /// This function is unsafe because it god damn well should be
    pub unsafe fn new() -> Self {
        Self {
            addr: None,
            virt_base: None,
            advanced: 0, // ((this - 1) * 4096) + addr = end frame address
        }
    }

    /// Sets physical addr if original VeryUnsafeFrameAllocator is dropped
    ///
    /// This function will panic if addr has already been set
    ///
    /// This function is unsafe because it potentially violates memory safety
    pub unsafe fn set_geom(&mut self, addr: PhysAddr, advance: usize) {
        if let None = self.addr {
            self.addr = Some(addr);
            self.advanced = advance
        } else {
            panic!("Cannot reassign physical address")
        }
    }

    pub fn get_advance(&self) -> usize {
        self.advanced
    }

    /// Allocates a range of physical frames to virtual memory via `self.advance`
    ///
    /// this works via calling `self.advance` do its caveats apply
    pub unsafe fn map_frames_from_range(
        &mut self,
        phy_addr_range: PhysFrameRangeInclusive,
        mapper: &mut OffsetPageTable,
    ) -> Option<VirtAddr> {
        self.set_geom(phy_addr_range.start.start_address(), 0);

        let mut virt_addr_base = None;

        for frame in phy_addr_range {
            if let None = virt_addr_base {
                virt_addr_base = self.advance(frame.start_address(), mapper);
            } else {
                self.advance(frame.start_address(), mapper);
            }
        }
        virt_addr_base
    }

    /// Maps a specified physical frame to a virtual page.
    /// Sets self.addr to an address with the last frame allocated
    ///

    pub unsafe fn get_frame(
        &mut self,
        phy_addr: PhysAddr,
        mapper: &mut OffsetPageTable,
    ) -> Option<VirtAddr> {
        // be careful with this

        self.set_geom(phy_addr, 0);

        self.advance(phy_addr, mapper)
    }

    /// Allocates memory by calling mapper.map_to
    ///
    /// In theory this only needs to be used to access memory written
    /// to by the firmware as such the writable bit is unset
    ///
    /// ##Panics
    /// this function will panic if it exceeds 2Mib of allocations
    /// because it cannot guarantee that the next page is free
    ///
    /// ##Saftey
    /// This function is unsafe because it violates memory safety
    pub unsafe fn advance(
        &mut self,
        phy_addr: PhysAddr,
        mapper: &mut OffsetPageTable,
    ) -> Option<VirtAddr> {
        return if let Some(page) = Self::find_unused_high_half(mapper) {
            mapper
                .map_to(
                    page,
                    PhysFrame::containing_address(phy_addr),
                    PageTableFlags::PRESENT | PageTableFlags::NO_EXECUTE,
                    self,
                )
                .unwrap()
                .flush();
            self.addr = None;
            Some(page.start_address())
        } else {
            None
        };
    }

    /// Unmaps Virtual page
    ///
    /// This function is unsafe because it can be used to unmap
    /// arbitrary Virtual memory
    pub unsafe fn dealloc_page(page: Page, mapper: &mut OffsetPageTable) {
        mapper.unmap(page).unwrap().1.flush();
    }

    /// Unmaps a range of Virtual pages
    ///
    /// This function is unsafe because it can be used to unmap
    /// arbitrary Virtual memory
    pub unsafe fn dealloc_pages(pages: PageRangeInclusive, mapper: &mut OffsetPageTable) {
        for page in pages {
            mapper.unmap(page).unwrap().1.flush();
        }
    }

    /// Searches fo an unused page within the high half of memory
    ///
    /// currently only scans for unused L3 page tables
    fn find_unused_high_half(mapper: &mut OffsetPageTable) -> Option<Page> {
        // todo make this better by traversing to L1 page tables
        for (e, p3) in mapper.level_4_table().iter().enumerate().skip(255) {
            if p3.is_unused() {
                return Some(Page::containing_address(VirtAddr::new((e << 39) as u64)));
                //conversion into 512GiB
            }
        }
        return None;
    }
}

unsafe impl FrameAllocator<Size4KiB> for VeryUnsafeFrameAllocator {
    //jesus christ this is bad
    //be careful with this
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let frame: PhysFrame<Size4KiB> = PhysFrame::containing_address(
            self.addr
                .expect("VeryUnsafeFrameAllocator failed addr not set")
                + (4096 * self.advanced),
        );
        self.advanced += 1;
        Some(frame)
    }
}

#[derive(Clone, Copy)]
struct PageIterator {
    pub start: Page,
    pub end: Page,
}

impl PageIterator {
    /// skips to the next l2 index pretending that next() was never called
    fn skip_l2(&mut self) {
        let mut n = self.start;

        let base = n.start_address().as_u64();
        if let Some(val) = base.checked_add(0x200000u64) {
            n = Page::containing_address(VirtAddr::new(val));
        } else {
            self.start = self.end;
            return;
        }

        if n > self.end {
            self.start = self.end
        } else {
            self.start = n
        }
    }

    fn skip_l3(&mut self) {
        let mut n = self.start;

        let base = n.start_address().as_u64();
        if let Some(val) = base.checked_add(0x40000000u64) {
            n = Page::containing_address(VirtAddr::new(val));
        } else {
            self.start = self.end;
            return;
        }

        if n > self.end {
            self.start = self.end
        } else {
            self.start = n
        }
    }

    fn skip_l4(&mut self) {
        let mut n = self.start;

        let base = n.start_address().as_u64();
        if let Some(val) = base.checked_add(0x8000000000u64) {
            n = Page::containing_address(VirtAddr::new(val));
        } else {
            self.start = self.end;
            return;
        }

        if n > self.end {
            self.start = self.end
        } else {
            self.start = n
        }
    }
    fn step_back(&mut self) {
        let n = self.start;
        let n = Page::containing_address(n.start_address() - 0x1000u64);

        if n > self.end {
            self.start = self.end
        }
        self.start = n
    }
}

impl Iterator for PageIterator {
    type Item = Page;

    fn next(&mut self) -> Option<Self::Item> {
        let ret = self.start;

        if ret == self.end {
            return None;
        }

        let mut n = ret.start_address().as_u64();
        n = n.checked_add(PAGE_SIZE as u64)?;

        self.start = Page::containing_address(VirtAddr::new(n));
        return Some(ret);
    }
}

pub(crate) const fn addr_from_indices(l4: usize, l3: usize, l2: usize, l1: usize) -> usize {
    const BITS_PAGE_OFFSET: u8 = 12;
    const BITS_TABLE_OFFSET: u8 = 9;

    assert!(l1 < 512);
    assert!(l2 < 512);
    assert!(l3 < 512);
    assert!(l4 < 512);

    let mut ret = l1 << BITS_PAGE_OFFSET;

    ret |= l2 << BITS_PAGE_OFFSET + (BITS_TABLE_OFFSET * 1);
    ret |= l3 << BITS_PAGE_OFFSET + (BITS_TABLE_OFFSET * 2);
    ret |= l4 << BITS_PAGE_OFFSET + (BITS_TABLE_OFFSET * 3);

    ret
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PageTableLevel {
    L1,
    L2,
    L3,
    L4,
}

impl PageTableLevel {
    /// Decrements Self downward
    pub const fn dec(self) -> Self {
        use self::PageTableLevel::*;
        match self {
            L1 => panic!(),
            L2 => L1,
            L3 => L2,
            L4 => L3,
        }
    }

    /// Increments Self upward
    pub const fn inc(self) -> Self {
        use self::PageTableLevel::*;
        match self {
            L1 => L2,
            L2 => L3,
            L3 => L4,
            L4 => panic!(),
        }
    }

    pub fn get_index<S: PageSize>(
        &self,
        page: Page<S>,
    ) -> x86_64::structures::paging::PageTableIndex {
        // this should compile to nothing required to use with all S types
        let page = unsafe { Page::from_start_address_unchecked(page.start_address()) };
        use self::PageTableLevel::*;
        return match self {
            L1 => page.p1_index(),
            L2 => page.p2_index(),
            L3 => page.p3_index(),
            L4 => page.p4_index(),
        };
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug, Default)]
/// MemRegion represents different regions of physical memory by the number of bits they require to access.
/// Some hardware devices can only use 32bit or more rarely 16bit addresses when accessing memory.
/// This enum is used to differentiate between them.
// impls are in buddy_frame_alloc.rs because they are only usd within that mod
//
// Defaults are not set here for other ptr sizes to avoid potential architecture weirdness
pub enum MemRegion {
    Mem16,
    Mem32,
    #[cfg_attr(target_pointer_width = "64",default)]
    Mem64,
}