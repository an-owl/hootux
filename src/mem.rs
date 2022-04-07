use bootloader::boot_info::{MemoryRegion, MemoryRegionKind};
use x86_64::structures::paging::frame::PhysFrameRangeInclusive;
use x86_64::structures::paging::page::PageRangeInclusive;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{structures::paging::PageTable, PhysAddr, VirtAddr};

/// A FrameAllocator that returns usable frames from the bootloader's memory map.
pub struct BootInfoFrameAllocator {
    memory_map: &'static [MemoryRegion],
    next: usize,
}

impl BootInfoFrameAllocator {
    /// Create a FrameAllocator from the passed memory map.
    ///
    /// This function is unsafe because the caller must guarantee that the passed
    /// memory map is valid. The main requirement is that all frames that are marked
    /// as `USABLE` in it are really unused.
    pub unsafe fn init(memory_map: &'static [MemoryRegion]) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
        }
    }

    /// Returns an iterator over the usable frames specified in the memory map.
    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        let regions = self.memory_map.iter();
        let usable_regions = regions.filter(|r| r.kind == MemoryRegionKind::Usable);

        let addr_ranges = usable_regions.map(|r| r.start..r.end);

        let frame_addresses = addr_ranges.flat_map(|r| r.step_by(4096));

        frame_addresses.map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
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
pub unsafe fn init(offset: VirtAddr) -> OffsetPageTable<'static> {
    let l4 = active_l4_table(offset);
    OffsetPageTable::new(l4, offset)
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

pub mod page_table_tree {
    use alloc::boxed::Box;
    use core::mem::MaybeUninit;
    use core::ops::{Index, IndexMut};
    use x86_64::instructions::tlb::flush_all;
    use x86_64::structures::paging::{
        Mapper, Page, PageTable, PageTableFlags, PageTableIndex, Size4KiB,
    };
    use x86_64::{PhysAddr, VirtAddr};
    use PageTableLevel::*;

    #[derive(Clone, Copy, PartialEq, Debug)]
    pub enum PageTableLevel {
        L1,
        L2,
        L3,
        L4,
    }

    /// Stores virtual addresses of Page Tables for reference
    ///
    /// Addresses contained may be Uninitialized check
    /// PageTableFlags::Present flag on relevant page table.
    /// Unlike a PageTable this does not contain flags.
    ///
    /// All entries are uninitialized until mapped
    #[repr(align(4096))]
    struct VirtualPageTable {
        tables: [MaybeUninit<Box<PageTableBranch>>; 512],
    }

    impl VirtualPageTable {
        /// Initializes a a nwe instance of VirtualPageTable
        fn new() -> Self {
            Self {
                tables: [const { MaybeUninit::uninit() }; 512], //Copy is not for Box so this is used instead
            }
        }

        fn set(&mut self, index: PageTableIndex, addr: Box<PageTableBranch>) {
            let index = usize::from(index);
            self.tables[index].write(addr);
        }
    }

    impl Index<PageTableIndex> for VirtualPageTable {
        type Output = MaybeUninit<Box<PageTableBranch>>;

        /// Returns an &MaybeUninit<PageTableBranch>
        /// that be initialized later

        fn index(&self, index: PageTableIndex) -> &Self::Output {
            let t = &self.tables[usize::from(index)];
            t
        }
    }

    impl IndexMut<PageTableIndex> for VirtualPageTable {
        fn index_mut(&mut self, index: PageTableIndex) -> &mut Self::Output {
            let t = &mut self.tables[usize::from(index)];
            t
        }
    }

    /// Contains PageTable metadata and virtual addresses of mapped pages.
    /// The contained PageTable and its mapped virtual addresses are
    /// stored within Box's.
    ///
    /// PageTableBranches with `level: PageTableLevel::L1` does not
    /// contain its mapped virtual addresses
    // editors note HUGE pages do not require a mapped virtual address
    // however they are already there taking space so use them?
    pub struct PageTableBranch {
        level: PageTableLevel,
        page: Box<PageTable>,
        virt_table: Option<Box<VirtualPageTable>>, // parent_entry: Option<PageTableIndex>?
                                                   // todo fast_drop: bool,
                                                   // fast drop will be set on children when the
                                                   // parent is dropped to skip unbinding steps
    }

    impl PageTableBranch {
        /// Creates a new PageTableBranch for level
        pub fn new(level: PageTableLevel) -> Self {
            match level {
                L1 => Self {
                    level,
                    page: Box::new(PageTable::new()),
                    virt_table: None,
                },
                _ => Self {
                    level,
                    page: Box::new(PageTable::new()),
                    virt_table: Some(Box::new(VirtualPageTable::new())),
                },
            }
        }

        pub fn level(&self) -> PageTableLevel {
            self.level
        }

        /// finds the first free index in self
        // TODO make one that searches within range
        pub fn find_free(&self) -> Option<PageTableIndex> {
            for (i, entry) in self.page.iter().enumerate() {
                if entry.is_unused() {
                    return Some(PageTableIndex::new(i as u16));
                }
            }
            None
        }

        /// Creates a child and binds it to index
        ///
        /// This function will panic if called on a PageTableBranch marked L1
        pub fn child(
            &mut self,
            index: PageTableIndex,
            mapper: &impl Mapper<Size4KiB>,
        ) -> &mut PageTableBranch {
            let level = match self.level {
                L1 => panic!("Tried to spawn child from L1 page table"),
                L2 => L1,
                L3 => L2,
                L4 => L3,
            };

            // im pretty sure maybe uninit is needed here
            let new_child = Box::new(Self::new(level));

            // assign to virt_table
            self.virt_table.as_mut().unwrap().set(index, new_child);
            flush_all();

            // get PhysAddr of child and map to self
            // child is now stored in self.virt_table
            let phys = unsafe { &*self.virt_table.as_mut().as_ref().unwrap()[index].as_ptr() }
                .page_phy_addr(mapper);

            self.page[index].set_addr(
                phys,
                PageTableFlags::PRESENT | PageTableFlags::NO_EXECUTE | PageTableFlags::WRITABLE,
            );

            unsafe { &mut *self.virt_table.as_mut().unwrap()[index].as_mut_ptr() }
        }

        /// Returns a reference to the contained PageTable
        pub fn get_page(&self) -> &PageTable {
            &*self.page
        }

        /// Returns the given page table as mutable
        pub fn get_page_mut(&mut self) -> &mut PageTable {
            &mut self.page
        }

        /// address of &self.page
        pub fn page_phy_addr(&self, mapper: &impl Mapper<Size4KiB>) -> PhysAddr {
            let virt_addr = VirtAddr::from_ptr(&*self.page);
            assert!(
                virt_addr.is_aligned(4096u64),
                "PageTable at {:#?} misaligned",
                virt_addr
            );

            mapper
                .translate_page(Page::containing_address(virt_addr))
                .unwrap()
                .start_address()
        }

        /// preforms a recursive [Prolicide](https://en.wikipedia.org/wiki/List_of_types_of_killing#Killing_of_family)
        /// on the child at `index`
        ///
        /// This function can be used to deallocate pages from L1 page tables
        ///
        /// This function is potentially expensive to call as it
        /// must drop all grandchildren
        pub fn drop_child(&mut self, index: PageTableIndex) {
            let curr_flags = self.page[index].flags();
            self.page[index].set_flags(curr_flags & !PageTableFlags::PRESENT);

            // drops given child if virt_table exists
            // virt table exists opn all but L1 page tables
            if let Some(vpt) = self.virt_table.as_mut() {
                unsafe { vpt[index].assume_init_drop() };
            }
        }

        /// Allocates a physical frame to the given page table index
        ///
        /// This function will panic if self.level is NOT L1
        pub fn allocate_frame(&mut self, index: PageTableIndex, addr: PhysAddr) {
            assert_eq!(self.level, L1);

            self.page[index].set_addr(
                addr,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
            )
        }

        /// Returns a mutable refrence to the child at `index`
        pub fn get_child_mut(&mut self, index: PageTableIndex) -> Option<&mut Self>{
            if let Some(vpt) = &mut self.virt_table{
                if self.page[index].is_unused() {
                    return None
                }
                let r = unsafe {&mut **vpt[index].as_mut_ptr()};
                return Some(r);
            } else { None }

        }
    }

    impl Drop for PageTableBranch {
        /// dropping should only be done form the parent
        /// because `drop()` cannot unmap itself from its parent
        fn drop(&mut self) {
            if self.level == L4 {
                // just in case
                // though
                panic!("tried to drop l4 page table")
            }
            let mut flags: [PageTableFlags; 512] = [PageTableFlags::empty(); 512];
            for (i, e) in self.page.iter().enumerate() {
                flags[i] = e.flags();
            }

            for (i, e) in flags.iter().enumerate() {
                if !e.is_empty() {
                    self.drop_child(PageTableIndex::new(i as u16));
                }
            }
        }
    }
}
