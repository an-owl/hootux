use super::PageTableLevel;
use super::PageTableLevel::*;
use super::PAGE_SIZE;
use crate::allocator::page_table_allocator::PtAlloc;
use crate::mem::{addr_from_indices, BootInfoFrameAllocator, PageIterator};
use alloc::boxed::Box;
use core::mem::MaybeUninit;
use core::ops::{Index, IndexMut};
use x86_64::structures::paging::mapper::{
    FlagUpdateError, MapToError, MapperFlush, MapperFlushAll, TranslateError, UnmapError,
};
use x86_64::structures::paging::{
    page::PageRangeInclusive, page_table::PageTableEntry, FrameAllocator, Mapper, Page, PageTable,
    PageTableFlags, PageTableIndex, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

/// This struct contains the Page Table tree and is used for Operations on memory.
/// PageTable Tree Branches are stored on the heap however Page Tables are stored
/// in memory at 0xff8000000000. On an L4 boundary this makes mapping slightly more efficient
pub struct PageTableTree {
    head: PageTableBranch,
}
impl PageTableTree {

    /// This function creates an instance of Self using an Offset Memory beginning at `phys_offset`
    /// an instance of the current mapper must be provided to look up physical frame addresses
    ///
    /// This function also initializes [hootux::alloc::page_table_allocator::PT_ALLOC]
    ///
    /// This function is unsafe because this function initializes PT_ALLOC it can only be called
    /// once doing so more than once will result in undefined behaviour. It also requires that the
    /// caller ensures that `phys_offset` points to the start of the offset physical memory.
    pub unsafe fn from_offset_page_table(
        phys_offset: VirtAddr,
        current_mapper: &mut impl Mapper<Size4KiB>,
        frame_alloc: &mut BootInfoFrameAllocator,
    ) -> Self {
        const PT_HEAP_START: VirtAddr =
            VirtAddr::new_truncate(addr_from_indices(511, 0, 0, 0) as u64);

        // calculate number of tables required to map `count` pages
        let tables_to_map = |count: usize| -> usize {
            let l1 = count.div_ceil(512);
            let l2 = l1.div_ceil(512);
            let l3 = l2.div_ceil(512);

            l1 + l2 + l3
        };

        let offset_table = super::offset_page_table::OffsetPageTable::new(phys_offset);
        let table_count = offset_table.count_tables() + 3;

        // size in pages
        let heap_size;

        // calculate heap size
        {
            let mut old_count = table_count;
            let mut new_count = table_count + tables_to_map(old_count);
            // runs heap size calculation again after updating length
            // this should never run more than a few times even when number of tables is huge
            // todo add warning that this has run too many times like 10 or 15 or something
            while old_count != new_count {
                old_count = new_count;
                new_count = table_count + tables_to_map(old_count);
            }
            heap_size = new_count
        }

        let heap_start = VirtAddr::new(super::addr_from_indices(511, 0, 0, 0) as u64);
        let heap_start_page = Page::containing_address(heap_start);

        let pages = PageRangeInclusive {
            start: heap_start_page,
            end: Page::containing_address(heap_start + (heap_size * PAGE_SIZE)),
        };

        for page in pages {
            let flags =
                PageTableFlags::PRESENT | PageTableFlags::NO_EXECUTE | PageTableFlags::WRITABLE;
            let frame = frame_alloc.allocate_frame().unwrap(); // cant boot anyway if this fails here
            current_mapper
                .map_to(page, frame, flags, frame_alloc)
                .unwrap()
                .flush();
        }

        crate::allocator::page_table_allocator::PT_ALLOC
            .lock()
            .init(PT_HEAP_START, PT_HEAP_START + (PAGE_SIZE * heap_size));

        let mut head = PageTableBranch::new(L4);

        let mut range = PageIterator {
            start: Page::containing_address(VirtAddr::new(0)),
            end: Page::containing_address(VirtAddr::new(
                super::addr_from_indices(511, 511, 511, 511) as u64,
            )),
        };
        while let Some((reference, new_range)) = offset_table.get_allocated_frames_within(range) {
            range = new_range;

            // TODO consolidate all the level enum into PageTableLevel

            head.set_page(
                Page::containing_address(reference.page),
                reference.entry,
                reference.size,
                current_mapper,
            );
        }

        Self { head }
    }

    /// sets the `cr3` register to the physical address of the contained l4 page table
    pub unsafe fn set_cr3(&self, mapper: &impl Mapper<Size4KiB>) {
        let flags = x86_64::registers::control::Cr3::read().1;

        x86_64::registers::control::Cr3::write(
            PhysFrame::containing_address(self.head.page_phy_addr(mapper)),
            flags,
        )
    }
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
    page: Box<PageTable, PtAlloc>,
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
                page: Box::new_in(PageTable::new(), PtAlloc),
                virt_table: None,
            },
            _ => Self {
                level,
                page: Box::new_in(PageTable::new(), PtAlloc),
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

    /// Creates a child and binds it to index. this does not flush the TLB
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

        // get PhysAddr of child and map to self
        // child is now stored in self.virt_table
        let phys = unsafe { &*self.virt_table.as_mut().as_ref().unwrap()[index].as_ptr() }
            .page_phy_addr(mapper);

        self.page[index].set_addr(phys, PageTableFlags::PRESENT | PageTableFlags::WRITABLE);

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
    /// This function will panic if self.level is L4
    pub fn allocate_frame(&mut self, index: PageTableIndex, frame: PhysFrame) {
        assert_ne!(self.level, PageTableLevel::L4);
        let flags;
        if let L1 = self.level {
            flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        } else {
            flags = PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::NO_EXECUTE
                | PageTableFlags::HUGE_PAGE
        }

        self.page[index].set_frame(frame, flags)
    }

    /// Returns a mutable reference to the child at `index`
    pub fn get_child_mut(&mut self, index: PageTableIndex) -> Option<&mut Self> {
        if let Some(vpt) = &mut self.virt_table {
            if self.page[index].is_unused() {
                return None;
            }
            let r = unsafe { &mut **vpt[index].as_mut_ptr() };
            return Some(r);
        } else {
            None
        }
    }

    pub fn get_child(&self, index: PageTableIndex) -> Option<&Self> {
        if let Some(vpt) = &self.virt_table {
            if self.page[index].is_unused() {
                return None;
            }
            let r = unsafe { & **vpt[index].as_ptr() };
            return Some(r);
        } else {
            None
        }
    }

    /// sets a the given entry to the provided one.
    ///
    /// this function will panic under several conditions.
    ///  * if is is not called on an L4 table
    ///  * if `level` is L4
    ///  * if `level` is L1 and entry.flags() contains HUGE_PAGE
    ///  * if `level` is L3, or L2 and entry.flags() does not contain HUGE_PAGE
    ///  * if an index in `page` below `level` is not 0
    unsafe fn set_page(
        &mut self,
        page: Page,
        entry: PageTableEntry,
        level: PageTableLevel,
        mapper: &impl Mapper<Size4KiB>,
    ) -> Option<PageTableEntry> {
        // sanity checks
        assert_eq!(self.level, L4, "tried to set_page() on {:?} table", entry);
        assert_ne!(level, L4);

        match level {
            L1 => {
                assert!(!entry.flags().contains(PageTableFlags::HUGE_PAGE))
            }
            L2 => {
                assert!(!entry.flags().contains(PageTableFlags::HUGE_PAGE));
                assert_eq!(page.p1_index(), PageTableIndex::new_truncate(0));
            }
            L3 => {
                assert!(!entry.flags().contains(PageTableFlags::HUGE_PAGE));
                assert_eq!(page.p1_index(), PageTableIndex::new_truncate(0));
                assert_eq!(page.p2_index(), PageTableIndex::new_truncate(0));
            }
            L4 => {
                panic!("PageTableBranch::set_page() Not allowed with `level` l4")
            }
        }

        let ret = self.set_page_inner(page, entry, level, mapper);
        ret
    }

    /// Sets the entry at the given level and index to the given entry.
    ///
    /// This should **NEVER** be called manually and should only be done through a function wrapper
    ///
    /// it is very unsafe because if `target_level` is not L1 and `entry` is present and *not* huge_page
    /// this will almost definitely cause an invalid pointer dereference.
    unsafe fn set_page_inner(
        &mut self,
        page: Page,
        entry: PageTableEntry,
        target_level: PageTableLevel,
        mapper: &impl Mapper<Size4KiB>,
    ) -> Option<PageTableEntry> {
        let index = self.level.get_index(page);
        let mut ret = None;
        if self.level == target_level {
            if !self.page.index(index).is_unused() {
                ret = Some(self.page.index(index).clone());
            }
            *self.page.index_mut(index) = entry;
        } else {
            let lower = self.get_or_create_child(index, mapper);
            lower.set_page_inner(page, entry, target_level, mapper);
        }
        ret
    }

    /// this will only ever need to be used if changing something so there is not immutable version
    unsafe fn get_or_create_child(
        &mut self,
        index: PageTableIndex,
        mapper: &impl Mapper<Size4KiB>,
    ) -> &mut PageTableBranch {
        if self.is_child_present(index) {
            self.get_child_mut(index).unwrap()
        } else {
            self.child(index, mapper)
        }
    }

    /// checks if a child is present at `index`
    pub fn is_child_present(&self, index: PageTableIndex) -> bool {
        if let L1 = self.level {
            return false;
        }

        let flags = self.page.index(index).flags();
        if flags.contains(PageTableFlags::PRESENT) && !flags.contains(PageTableFlags::HUGE_PAGE) {
            return true;
        }

        return false;
    }

    fn get_entry(&self, index: PageTableIndex) -> PageTableEntry{
        self.page.index(index).clone()
    }

    fn set_unused(&mut self, index: PageTableIndex) {
        self.page.index(index).set_unused();
    }
}

impl Drop for PageTableBranch {
    /// dropping should only be done form the parent
    /// because `drop()` cannot unmap itself from its parent
    fn drop(&mut self) {
        if self.level == L4 {
            // just in case
            // though
            // TODO remove this. dropping L4 tables will be necessary for user mode
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
