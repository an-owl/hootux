use super::PageTableLevel;
use super::PageTableLevel::*;
use super::PAGE_SIZE;
use crate::allocator::page_table_allocator::PtAlloc;
use crate::mem::{addr_from_indices, BootInfoFrameAllocator, PageIterator};
use alloc::boxed::Box;
use core::fmt::{Debug, Formatter};
use core::mem::MaybeUninit;
use core::ops::{Index, IndexMut};
use x86_64::structures::paging::mapper::{FlagUpdateError, MapToError, MapperFlush, MapperFlushAll, TranslateError, UnmapError, MapperAllSizes};
use x86_64::structures::paging::page_table::FrameError;
use x86_64::structures::paging::{
    page::PageRangeInclusive, page_table::PageTableEntry, FrameAllocator, Mapper, Page, PageSize,
    PageTable, PageTableFlags, PageTableIndex, PhysFrame, Size1GiB, Size2MiB, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

//TODO unify metrics (i.e page/frame or address)

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
        const PTALLOC_EXTRA_PAGES: usize = 20;

        // calculate number of tables required to map `count` pages
        let tables_to_map = |count: usize| -> usize {
            let l1 = count.div_ceil(512);
            let l2 = l1.div_ceil(512);
            let l3 = l2.div_ceil(512);

            l1 + l2 + l3
        };

        let offset_table = super::offset_page_table::OffsetPageTable::new(phys_offset);
        let table_count = offset_table.count_tables() + PTALLOC_EXTRA_PAGES;

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

        let heap_start = VirtAddr::new(addr_from_indices(511, 0, 0, 0) as u64);
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
                addr_from_indices(511, 511, 511, 511) as u64,
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

    /// Returns the table at `level` that contains the address in `page`
    ///
    /// This function does not check the alignment of `page`
    pub fn traverse<S: PageSize>(
        &self,
        page: Page<S>,
        level: PageTableLevel,
    ) -> Result<&PageTableBranch, FrameError> {
        let page = Page::<Size4KiB>::containing_address(page.start_address()); // required to handle Page<!Size4Kib>

        let mut table = &self.head;

        // returns l4 table
        if let L4 = level {
            // https://www.youtube.com/watch?v=y2weNM4JtME&ab_channel=JonTronShow
            return Ok(table);
        }

        if !table.is_child_present(page.p4_index()) {
            return Err(FrameError::FrameNotPresent);
        }
        table = &table.get_child(page.p4_index()).unwrap();

        // returns l3 table
        if let L3 = level {
            return Ok(table);
        }

        if !table.is_child_present(page.p3_index()) {
            return if table.get_entry(page.p3_index()).flags().is_empty() {
                Err(FrameError::FrameNotPresent)
            } else {
                Err(FrameError::HugeFrame)
            };
        }
        table = &table.get_child(page.p3_index()).unwrap();

        // return l2 table
        if let L2 = level {
            return Ok(table);
        }
        if !table.is_child_present(page.p2_index()) {
            return if table.get_entry(page.p2_index()).flags().is_empty() {
                Err(FrameError::FrameNotPresent)
            } else {
                Err(FrameError::HugeFrame)
            };
        }

        table = &table.get_child(page.p2_index()).unwrap();

        // return l1 table
        Ok(table)
    }

    fn traverse_mut<S: PageSize>(
        &mut self,
        page: Page<S>,
        level: PageTableLevel,
    ) -> Result<&mut PageTableBranch, FrameError> {
        let page = Page::<Size4KiB>::containing_address(page.start_address()); // required to handle Page<!Size4Kib>

        let mut table = &mut self.head;

        if let L4 = level {
            return Ok(table);
        }

        // check child exists
        if !table.is_child_present(L4.get_index(page)) {
            // thy not
            if table.get_entry(L4.get_index(page)).is_unused() {
                return Err(FrameError::FrameNotPresent);
            } else if table
                .get_entry(L4.get_index(page))
                .flags()
                .contains(PageTableFlags::HUGE_PAGE)
            {
                return Err(FrameError::HugeFrame);
            } else {
                panic!()
            } // if this triggers then PageTableBranch::is_child_present() now checks more than PRESENT & HUGE_PAGE
        } else {
            table = table.get_child_mut(L4.get_index(page)).unwrap()
        }

        if let L3 = level {
            return Ok(table);
        }

        if !table.is_child_present(L3.get_index(page)) {
            if table.get_entry(L3.get_index(page)).is_unused() {
                return Err(FrameError::FrameNotPresent);
            } else if table
                .get_entry(L3.get_index(page))
                .flags()
                .contains(PageTableFlags::HUGE_PAGE)
            {
                return Err(FrameError::HugeFrame);
            } else {
                panic!()
            } // if this triggers then PageTableBranch::is_child_present() now checks more than PRESENT & HUGE_PAGE
        } else {
            table = table.get_child_mut(L3.get_index(page)).unwrap()
        }

        if let L2 = level {
            return Ok(table);
        }

        if !table.is_child_present(L2.get_index(page)) {
            if table.get_entry(L2.get_index(page)).is_unused() {
                return Err(FrameError::FrameNotPresent);
            } else if table
                .get_entry(L2.get_index(page))
                .flags()
                .contains(PageTableFlags::HUGE_PAGE)
            {
                return Err(FrameError::HugeFrame);
            } else {
                panic!()
            } // if this triggers then PageTableBranch::is_child_present() now checks more than PRESENT & HUGE_PAGE
        } else {
            table = table.get_child_mut(L2.get_index(page)).unwrap()
        }

        Ok(table)
    }

    fn gen_child(&self) -> (Box<PageTable, PtAlloc>, PhysFrame) {
        let table = Box::new_in(PageTable::new(), PtAlloc);
        let table_virt_addr = VirtAddr::from_ptr(table.as_ref());
        //ensure that this works with huge pages
        let table_phys_addr;

        if let Ok(table_frame) =
            self.translate_page(Page::<Size4KiB>::containing_address(table_virt_addr))
        {
            table_phys_addr = table_frame.start_address()
        } else {
            // if HUGE try all HUGE sizes
            if let Ok(table_frame) =
                self.translate_page(Page::<Size2MiB>::containing_address(table_virt_addr))
            {
                table_phys_addr = table_frame.start_address()
            } else {
                table_phys_addr = self
                    .translate_page(Page::<Size1GiB>::containing_address(table_virt_addr))
                    .unwrap()
                    .start_address();
            }
        }

        (table, PhysFrame::containing_address(table_phys_addr))
    }

    /// Checks stored tree to see if the table containing `page` exists at `level`.
    /// If it does returns `Ok(())`, otherwise returns the error encountered while
    /// retrieving branch and the level that could not be reached.
    fn traversable<S: PageSize>(
        &self,
        page: Page<S>,
        level: PageTableLevel,
    ) -> Result<(), (FrameError, PageTableLevel)> {
        let page = Page::<Size4KiB>::containing_address(page.start_address()); // required to handle Page<!Size4Kib>
                                                                               // some of this is redundant and can be removed but i don't tant to do it now
        let mut current_table = &self.head;

        if let L4 = level {
            // see Self::traverse:5
            return Ok(());
        }

        // check current_table@L4 for l3 child
        if current_table.is_child_present(L4.get_index(page)) {
            if let L4 = level {
                return Ok(());
            }
            current_table = current_table.get_child(L4.get_index(page)).unwrap()
        } else {
            if current_table.get_entry(L4.get_index(page)).is_unused() {
                return Err((FrameError::FrameNotPresent, L3));
            } else if current_table
                .get_entry(L4.get_index(page))
                .flags()
                .contains(PageTableFlags::HUGE_PAGE)
            {
                return Err((FrameError::HugeFrame, L3));
            } else {
                panic!()
            }
        }

        // check current_table@L3 for l2 child
        if current_table.is_child_present(L3.get_index(page)) {
            if let L3 = level {
                return Ok(());
            }
            current_table = current_table.get_child(L3.get_index(page)).unwrap()
        } else {
            if current_table.get_entry(L3.get_index(page)).is_unused() {
                return Err((FrameError::FrameNotPresent, L2));
            } else if current_table
                .get_entry(L3.get_index(page))
                .flags()
                .contains(PageTableFlags::HUGE_PAGE)
            {
                return Err((FrameError::HugeFrame, L2));
            } else {
                panic!()
            }
        }

        // check current_table@L2 for l1 child
        if current_table.is_child_present(L2.get_index(page)) {
            if let L2 = level {
                return Ok(());
            }
        } else {
            if current_table.get_entry(L2.get_index(page)).is_unused() {
                return Err((FrameError::FrameNotPresent, L1));
            } else if current_table
                .get_entry(L2.get_index(page))
                .flags()
                .contains(PageTableFlags::HUGE_PAGE)
            {
                return Err((FrameError::HugeFrame, L1));
            } else {
                panic!()
            }
        }

        // L1 child is always ok
        Ok(())
    }
}

impl Mapper<Size4KiB> for PageTableTree {
    /// this will never call _frame_allocator
    unsafe fn map_to_with_table_flags<A>(
        &mut self,
        page: Page<Size4KiB>,
        frame: PhysFrame<Size4KiB>,
        flags: PageTableFlags,
        parent_table_flags: PageTableFlags,
        _frame_allocator: &mut A,
    ) -> Result<MapperFlush<Size4KiB>, MapToError<Size4KiB>>
    where
        Self: Sized,
        A: FrameAllocator<Size4KiB> + ?Sized,
    {
        const LEVEL: PageTableLevel = L1;

        match self.traversable(page, LEVEL) {
            Ok(_) => {}
            Err((FrameError::FrameNotPresent, mut level)) => {
                loop {
                    let child_bits = self.gen_child();
                    let bottom_table = self.traverse_mut(page, level.inc()).unwrap(); // should never panic
                    bottom_table.child_from_table(
                        level.inc().get_index(page),
                        child_bits.0,
                        child_bits.1.start_address(),
                    );

                    if level == LEVEL {
                        break;
                    } // if level == l1 break, continuing will panic
                    level = level.dec();
                }
            }
            Err((FrameError::HugeFrame, _)) => return Err(MapToError::ParentEntryHugePage),
        }
        {
            let target_parent = self.traverse_mut(page, LEVEL.inc()).unwrap();
            target_parent
                .get_entry_mut(LEVEL.inc().get_index(page))
                .set_flags(parent_table_flags)
        }
        // should be guaranteed to exist
        let target_table = self.traverse_mut(page, LEVEL).unwrap();
        return if target_table.get_entry(LEVEL.get_index(page)).is_unused() {
            target_table.allocate_frame_with_flags(LEVEL.get_index(page), frame, flags);
            Ok(MapperFlush::new(page))
        } else {
            Err(MapToError::PageAlreadyMapped(
                PhysFrame::containing_address(target_table.get_entry(LEVEL.get_index(page)).addr()),
            ))
        };
    }

    fn unmap(
        &mut self,
        page: Page<Size4KiB>,
    ) -> Result<(PhysFrame<Size4KiB>, MapperFlush<Size4KiB>), UnmapError> {
        const LEVEL: PageTableLevel = L1;

        let target_table = match self.traverse_mut(page, LEVEL) {
            Ok(branch) => branch,
            Err(FrameError::HugeFrame) => return Err(UnmapError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => return Err(UnmapError::PageNotMapped),
        };

        match target_table.get_entry(LEVEL.get_index(page)).frame() {
            Ok(frame) => {
                target_table.set_unused(LEVEL.get_index(page));
                return Ok((frame, MapperFlush::new(page)));
            }
            Err(FrameError::HugeFrame) => panic!(),
            Err(FrameError::FrameNotPresent) => {
                return Err(UnmapError::PageNotMapped);
            }
        }
    }

    unsafe fn update_flags(
        &mut self,
        page: Page<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlush<Size4KiB>, FlagUpdateError> {
        const LEVEL: PageTableLevel = L1;

        return match self.traverse_mut(page, LEVEL) {
            Ok(table) => {
                table.get_entry_mut(LEVEL.get_index(page)).set_flags(flags);
                Ok(MapperFlush::new(page))
            }
            Err(FrameError::HugeFrame) => Err(FlagUpdateError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => Err(FlagUpdateError::PageNotMapped),
        };
    }

    unsafe fn set_flags_p4_entry(
        &mut self,
        page: Page<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = L4;

        let table = &mut self.head;

        if table.is_child_present(LEVEL.get_index(page)) {
            table.get_entry_mut(LEVEL.get_index(page)).set_flags(flags);
            Ok(MapperFlushAll::new())
        } else if table.get_entry(LEVEL.get_index(page)).flags().is_empty() {
            Err(FlagUpdateError::PageNotMapped)
        } else {
            Err(FlagUpdateError::ParentEntryHugePage)
        }
    }

    unsafe fn set_flags_p3_entry(
        &mut self,
        page: Page<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = L3;
        // basically a copy paste of `set_flags_p3_entry`

        return match self.traverse_mut(page, LEVEL) {
            Ok(table) => {
                if table.is_child_present(LEVEL.get_index(page)) {
                    table.get_entry_mut(LEVEL.get_index(page)).set_flags(flags);
                    Ok(MapperFlushAll::new())
                } else if table.get_entry(LEVEL.get_index(page)).flags().is_empty() {
                    Err(FlagUpdateError::PageNotMapped)
                } else {
                    Err(FlagUpdateError::ParentEntryHugePage)
                }
            }
            Err(FrameError::HugeFrame) => Err(FlagUpdateError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => Err(FlagUpdateError::PageNotMapped),
        };
    }

    unsafe fn set_flags_p2_entry(
        &mut self,
        page: Page<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = L2;
        // basically a copy paste of `set_flags_p3_entry`

        return match self.traverse_mut(page, LEVEL) {
            Ok(table) => {
                if table.is_child_present(LEVEL.get_index(page)) {
                    table.get_entry_mut(LEVEL.get_index(page)).set_flags(flags);

                    Ok(MapperFlushAll::new())
                } else if table.get_entry(LEVEL.get_index(page)).flags().is_empty() {
                    Err(FlagUpdateError::PageNotMapped)
                } else {
                    Err(FlagUpdateError::ParentEntryHugePage)
                }
            }
            Err(FrameError::HugeFrame) => Err(FlagUpdateError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => Err(FlagUpdateError::PageNotMapped),
        };
    }

    fn translate_page(&self, page: Page<Size4KiB>) -> Result<PhysFrame<Size4KiB>, TranslateError> {
        const LEVEL: PageTableLevel = L1;

        return match self.traverse(page, LEVEL) {
            Ok(table) => {
                let entry = table.get_entry(LEVEL.get_index(page));
                if entry.is_unused() {
                    Err(TranslateError::PageNotMapped)
                } else {
                    Ok(PhysFrame::containing_address(entry.addr()))
                }
            }
            Err(FrameError::HugeFrame) => Err(TranslateError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => Err(TranslateError::PageNotMapped),
        };
    }
}

impl Mapper<Size2MiB> for PageTableTree {
    unsafe fn map_to_with_table_flags<A>(
        &mut self,
        page: Page<Size2MiB>,
        frame: PhysFrame<Size2MiB>,
        flags: PageTableFlags,
        parent_table_flags: PageTableFlags,
        _frame_allocator: &mut A,
    ) -> Result<MapperFlush<Size2MiB>, MapToError<Size2MiB>>
    where
        Self: Sized,
        A: FrameAllocator<Size4KiB> + ?Sized,
    {
        const LEVEL: PageTableLevel = L2;

        match self.traversable(page, LEVEL) {
            Ok(_) => {}
            Err((FrameError::FrameNotPresent, mut level)) => {
                loop {
                    let child_bits = self.gen_child();
                    let bottom_table = self.traverse_mut(page, level.inc()).unwrap(); // should never panic
                    bottom_table.child_from_table(
                        level.inc().get_index(page),
                        child_bits.0,
                        child_bits.1.start_address(),
                    );

                    if level == LEVEL {
                        break;
                    } // if level == l1 break, continuing will panic
                    level = level.dec();
                }
            }
            Err((FrameError::HugeFrame, _)) => return Err(MapToError::ParentEntryHugePage),
        }
        {
            let target_parent = self.traverse_mut(page, LEVEL.inc()).unwrap();
            target_parent
                .get_entry_mut(LEVEL.inc().get_index(page))
                .set_flags(parent_table_flags)
        }
        // should be guaranteed to exist
        let target_table = self.traverse_mut(page, LEVEL).unwrap();
        return if target_table.get_entry(LEVEL.get_index(page)).is_unused() {
            target_table.allocate_frame_with_flags(LEVEL.get_index(page), frame, flags);
            Ok(MapperFlush::new(page))
        } else {
            Err(MapToError::PageAlreadyMapped(
                PhysFrame::containing_address(target_table.get_entry(LEVEL.get_index(page)).addr()),
            ))
        };
    }

    fn unmap(
        &mut self,
        page: Page<Size2MiB>,
    ) -> Result<(PhysFrame<Size2MiB>, MapperFlush<Size2MiB>), UnmapError> {
        const LEVEL: PageTableLevel = L2;

        let target_table = match self.traverse_mut(page, LEVEL) {
            Ok(branch) => branch,
            Err(FrameError::HugeFrame) => return Err(UnmapError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => return Err(UnmapError::PageNotMapped),
        };

        match target_table.get_entry(LEVEL.get_index(page)).frame() {
            Ok(frame) => {
                let frame =
                    unsafe { PhysFrame::from_start_address_unchecked(frame.start_address()) };
                target_table.set_unused(LEVEL.get_index(page));
                return Ok((frame, MapperFlush::new(page)));
            }
            Err(FrameError::HugeFrame) => panic!(),
            Err(FrameError::FrameNotPresent) => {
                return Err(UnmapError::PageNotMapped);
            }
        }
    }

    unsafe fn update_flags(
        &mut self,
        page: Page<Size2MiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlush<Size2MiB>, FlagUpdateError> {
        const LEVEL: PageTableLevel = L2;

        return match self.traverse_mut(page, LEVEL) {
            Ok(table) => {
                table.get_entry_mut(LEVEL.get_index(page)).set_flags(flags);
                Ok(MapperFlush::new(page))
            }
            Err(FrameError::HugeFrame) => Err(FlagUpdateError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => Err(FlagUpdateError::PageNotMapped),
        };
    }

    unsafe fn set_flags_p4_entry(
        &mut self,
        page: Page<Size2MiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = L4;
        let table = &mut self.head;

        if table.is_child_present(LEVEL.get_index(page)) {
            table.get_entry_mut(LEVEL.get_index(page)).set_flags(flags);
            Ok(MapperFlushAll::new())
        } else if table.get_entry(LEVEL.get_index(page)).flags().is_empty() {
            Err(FlagUpdateError::PageNotMapped)
        } else {
            Err(FlagUpdateError::ParentEntryHugePage)
        }
    }

    unsafe fn set_flags_p3_entry(
        &mut self,
        page: Page<Size2MiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = L3;
        // basically a copy paste of `set_flags_p3_entry`

        return match self.traverse_mut(page, LEVEL) {
            Ok(table) => {
                if table.is_child_present(LEVEL.get_index(page)) {
                    table.get_entry_mut(LEVEL.get_index(page)).set_flags(flags);
                    Ok(MapperFlushAll::new())
                } else if table.get_entry(LEVEL.get_index(page)).flags().is_empty() {
                    Err(FlagUpdateError::PageNotMapped)
                } else {
                    Err(FlagUpdateError::ParentEntryHugePage)
                }
            }
            Err(FrameError::HugeFrame) => Err(FlagUpdateError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => Err(FlagUpdateError::PageNotMapped),
        };
    }

    unsafe fn set_flags_p2_entry(
        &mut self,
        page: Page<Size2MiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = L2;
        // basically a copy paste of `set_flags_p3_entry`

        return match self.traverse_mut(page, LEVEL) {
            Ok(table) => {
                if table.is_child_present(LEVEL.get_index(page)) {
                    table.get_entry_mut(LEVEL.get_index(page)).set_flags(flags);
                    Ok(MapperFlushAll::new())
                } else if table.get_entry(LEVEL.get_index(page)).flags().is_empty() {
                    Err(FlagUpdateError::PageNotMapped)
                } else {
                    Err(FlagUpdateError::ParentEntryHugePage)
                }
            }
            Err(FrameError::HugeFrame) => Err(FlagUpdateError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => Err(FlagUpdateError::PageNotMapped),
        };
    }

    fn translate_page(&self, page: Page<Size2MiB>) -> Result<PhysFrame<Size2MiB>, TranslateError> {
        const LEVEL: PageTableLevel = L2;

        return match self.traverse(page, LEVEL) {
            Ok(table) => {
                let entry = table.get_entry(LEVEL.get_index(page));
                if entry.is_unused() {
                    Err(TranslateError::PageNotMapped)
                } else {
                    Ok(PhysFrame::containing_address(entry.addr()))
                }
            }
            Err(FrameError::HugeFrame) => Err(TranslateError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => Err(TranslateError::PageNotMapped),
        };
    }
}

impl Mapper<Size1GiB> for PageTableTree {
    unsafe fn map_to_with_table_flags<A>(
        &mut self,
        page: Page<Size1GiB>,
        frame: PhysFrame<Size1GiB>,
        flags: PageTableFlags,
        parent_table_flags: PageTableFlags,
        _frame_allocator: &mut A,
    ) -> Result<MapperFlush<Size1GiB>, MapToError<Size1GiB>>
    where
        Self: Sized,
        A: FrameAllocator<Size4KiB> + ?Sized,
    {
        const LEVEL: PageTableLevel = L3;

        match self.traversable(page, LEVEL) {
            Ok(_) => {}
            Err((FrameError::FrameNotPresent, mut level)) => {
                loop {
                    let child_bits = self.gen_child();
                    let bottom_table = self.traverse_mut(page, level.inc()).unwrap(); // should never panic
                    bottom_table.child_from_table(
                        level.inc().get_index(page),
                        child_bits.0,
                        child_bits.1.start_address(),
                    );

                    if level == LEVEL {
                        break;
                    } // if level == l1 break, continuing will panic
                    level = level.dec();
                }
            }
            Err((FrameError::HugeFrame, _)) => return Err(MapToError::ParentEntryHugePage),
        }
        {
            let target_parent = self.traverse_mut(page, LEVEL.inc()).unwrap();
            target_parent
                .get_entry_mut(LEVEL.inc().get_index(page))
                .set_flags(parent_table_flags)
        }
        // should be guaranteed to exist
        let target_table = self.traverse_mut(page, LEVEL).unwrap();
        return if target_table.get_entry(LEVEL.get_index(page)).is_unused() {
            target_table.allocate_frame_with_flags(LEVEL.get_index(page), frame, flags);
            Ok(MapperFlush::new(page))
        } else {
            Err(MapToError::PageAlreadyMapped(
                PhysFrame::containing_address(target_table.get_entry(LEVEL.get_index(page)).addr()),
            ))
        };
    }

    fn unmap(
        &mut self,
        page: Page<Size1GiB>,
    ) -> Result<(PhysFrame<Size1GiB>, MapperFlush<Size1GiB>), UnmapError> {
        const LEVEL: PageTableLevel = L3;

        let target_table = match self.traverse_mut(page, LEVEL) {
            Ok(branch) => branch,
            Err(FrameError::HugeFrame) => return Err(UnmapError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => return Err(UnmapError::PageNotMapped),
        };

        match target_table.get_entry(LEVEL.get_index(page)).frame() {
            Ok(frame) => {
                let frame =
                    unsafe { PhysFrame::from_start_address_unchecked(frame.start_address()) };
                target_table.set_unused(LEVEL.get_index(page));
                return Ok((frame, MapperFlush::new(page)));
            }
            Err(FrameError::HugeFrame) => panic!(),
            Err(FrameError::FrameNotPresent) => {
                return Err(UnmapError::PageNotMapped);
            }
        }
    }

    unsafe fn update_flags(
        &mut self,
        page: Page<Size1GiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlush<Size1GiB>, FlagUpdateError> {
        const LEVEL: PageTableLevel = L3;

        return match self.traverse_mut(page, LEVEL) {
            Ok(table) => {
                table.get_entry_mut(LEVEL.get_index(page)).set_flags(flags);
                Ok(MapperFlush::new(page))
            }
            Err(FrameError::HugeFrame) => Err(FlagUpdateError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => Err(FlagUpdateError::PageNotMapped),
        };
    }

    unsafe fn set_flags_p4_entry(
        &mut self,
        page: Page<Size1GiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = L4;

        let table = &mut self.head;

        if table.is_child_present(LEVEL.get_index(page)) {
            table.get_entry_mut(LEVEL.get_index(page)).set_flags(flags);
            Ok(MapperFlushAll::new())
        } else if table.get_entry(LEVEL.get_index(page)).flags().is_empty() {
            Err(FlagUpdateError::PageNotMapped)
        } else {
            Err(FlagUpdateError::ParentEntryHugePage)
        }
    }

    unsafe fn set_flags_p3_entry(
        &mut self,
        page: Page<Size1GiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = L3;
        // basically a copy paste of `set_flags_p3_entry`
        return match self.traverse_mut(page, LEVEL) {
            Ok(table) => {
                if table.is_child_present(LEVEL.get_index(page)) {
                    table.get_entry_mut(LEVEL.get_index(page)).set_flags(flags);
                    Ok(MapperFlushAll::new())
                } else if table.get_entry(LEVEL.get_index(page)).flags().is_empty() {
                    Err(FlagUpdateError::PageNotMapped)
                } else {
                    Err(FlagUpdateError::ParentEntryHugePage)
                }
            }
            Err(FrameError::HugeFrame) => Err(FlagUpdateError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => Err(FlagUpdateError::PageNotMapped),
        };
    }

    unsafe fn set_flags_p2_entry(
        &mut self,
        _page: Page<Size1GiB>,
        _flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        // ???
        Err(FlagUpdateError::ParentEntryHugePage)
    }

    fn translate_page(&self, page: Page<Size1GiB>) -> Result<PhysFrame<Size1GiB>, TranslateError> {
        const LEVEL: PageTableLevel = L3;

        return match self.traverse(page, LEVEL) {
            Ok(table) => {
                let entry = table.get_entry(LEVEL.get_index(page));
                if entry.is_unused() {
                    Err(TranslateError::PageNotMapped)
                } else {
                    Ok(PhysFrame::containing_address(entry.addr()))
                }
            }
            Err(FrameError::HugeFrame) => Err(TranslateError::ParentEntryHugePage),
            Err(FrameError::FrameNotPresent) => Err(TranslateError::PageNotMapped),
        };
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
    pub level: PageTableLevel,
    pub page: Box<PageTable, PtAlloc>,
    virt_table: Option<Box<VirtualPageTable>>, // parent_entry: Option<PageTableIndex>?
                                               // todo fast_drop: bool,
                                               // fast drop will be set on children when the
                                               // parent is dropped to skip unbinding steps
}

impl Debug for PageTableBranch {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "PageTableBranch {{ level: {:?}, table:\n", self.level).unwrap();
        for (i, e) in self.page.iter().enumerate() {
            writeln!(f, "{} {:?}", i, e).unwrap()
        }
        write!(f, "\n")
    }
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
        assert_ne!(self.level, L1);

        // im pretty sure maybe uninit is needed here
        let new_child = Box::new(Self::new(self.level.dec()));

        // assign to virt_table
        self.virt_table.as_mut().unwrap().set(index, new_child);

        // get PhysAddr of child and map to self
        // child is now stored in self.virt_table
        let phys = unsafe { &*self.virt_table.as_mut().as_ref().unwrap()[index].as_ptr() }
            .page_phy_addr(mapper);

        self.page[index].set_addr(phys, PageTableFlags::PRESENT | PageTableFlags::WRITABLE);

        unsafe { &mut *self.virt_table.as_mut().unwrap()[index].as_mut_ptr() }
    }

    //TODO replace child with this (using the name child)
    /// Creates a child using an existing `Box<PageTable>`
    ///
    /// This is intended to be called from an `impl Mapper<S>`
    unsafe fn child_from_table(
        &mut self,
        index: PageTableIndex,
        table: Box<PageTable, PtAlloc>,
        table_addr: PhysAddr,
    ) {
        assert_ne!(self.level, L1);

        let new = Self {
            level: self.level.dec(),
            page: table,
            virt_table: Some(Box::new(VirtualPageTable::new())),
        };

        self.virt_table.as_mut().unwrap().set(index, Box::new(new));

        self.page[index].set_frame(
            PhysFrame::containing_address(table_addr),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );
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
    pub unsafe fn allocate_frame(&mut self, index: PageTableIndex, frame: PhysFrame) {
        assert_ne!(self.level, L4);
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

    /// Allocates a physical frame to the given page table index
    /// this function does not ensure that flags are set correctly
    /// the caller must ensure that PRESENT and HUGE_PAGE are set appropriately
    ///
    /// this function will panic if `self.level` is L4
    ///
    /// This function is unsafe because it breaks memory safety by potentially referencing a used
    /// physical frame. Potentially violating memory safety.
    /// The caller must also ensure that if self.level is L2 or L3 that `PageTableFlags::HUGE_PAGE`
    /// is set when PageTableFlags::PRESENT is set, failing to do so will cause a null pointer dereference
    unsafe fn allocate_frame_with_flags<S: PageSize>(
        &mut self,
        index: PageTableIndex,
        frame: PhysFrame<S>,
        flags: PageTableFlags,
    ) {
        let frame = PhysFrame::from_start_address_unchecked(frame.start_address());
        assert_ne!(self.level, L4);

        self.get_entry_mut(index).set_frame(frame, flags)
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
            let r = unsafe { &**vpt[index].as_ptr() };
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

    fn get_entry(&self, index: PageTableIndex) -> PageTableEntry {
        self.page[index].clone()
    }

    fn get_entry_mut(&mut self, index: PageTableIndex) -> &mut PageTableEntry {
        &mut self.page[index]
    }

    fn set_unused(&mut self, index: PageTableIndex) {
        self.page.index_mut(index).set_unused();
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
