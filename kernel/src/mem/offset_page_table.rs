use super::{PageIterator, PageTableLevel};
use x86_64::structures::paging::mapper::{
    FlagUpdateError, MapToError, MapperFlush, MapperFlushAll, TranslateError, UnmapError,
};
use x86_64::structures::paging::page_table::{FrameError, PageTableEntry};
use x86_64::structures::paging::{
    FrameAllocator, Mapper, Page, PageSize, PageTable, PageTableFlags, PhysFrame, Size1GiB,
    Size2MiB, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};
use super::tlb::{
    shootdown,
    shootdown_hint,
    ShootdownContent,
};

pub struct OffsetPageTable {
    offset_base: VirtAddr,
    l4_table: &'static mut PageTable,
}

#[derive(Copy, Clone, Debug)]
enum InternalError {
    ParentEntryHugePage(PageTableLevel),
    PageNotMapped(PageTableLevel),
    PageAlreadyMapped(PhysAddr),
}

impl OffsetPageTable {
    /// Initializes OffsetPageTable
    ///
    /// This function is unsafe because the caller must ensure that the
    /// given `offset_base` is correct
    pub(super) unsafe fn new(offset_base: VirtAddr) -> Self {
        let l4_frame = x86_64::registers::control::Cr3::read().0;
        let l4_table =
            &mut *(offset_base + l4_frame.start_address().as_u64()).as_mut_ptr::<PageTable>();

        Self {
            offset_base,
            l4_table,
        }
    }

    pub(crate) fn get_l4_table(&self) -> &PageTable {
        self.l4_table
    }

    /// Returns all mapped pages and their frames from `start` to `end`
    pub(super) fn get_allocated_frames_within(
        &self,
        mut range: PageIterator,
    ) -> Option<(PageReference, PageIterator)> {
        // this spaghetti needs some sauce but this is the best i can do
        // each page is iterated in order to get its frame entry
        while let Some(page) = range.next() {
            // each page in the loop represents 4k if a huge page is found this must handled
            // this is done by skipping each page in the loop

            // get l1 table of page so the table entry may be looked up
            match self.get_l1_for_addr(page.start_address()) {
                Ok(l1) => {
                    let entry = l1[page.p1_index()].clone();
                    if entry.is_unused() {
                        continue;
                    }

                    return Some((
                        PageReference {
                            page: page.start_address(),
                            size: PageTableLevel::L1,
                            entry,
                        },
                        range,
                    ));
                }

                // if no frame is found then continue because it does not need to be copied
                // must skip appropriately
                Err((FrameError::FrameNotPresent, PageTableLevel::L1)) => {
                    range.step_back();
                    range.skip_l2();
                }
                Err((FrameError::FrameNotPresent, PageTableLevel::L2)) => {
                    range.step_back();
                    range.skip_l3();
                }
                Err((FrameError::FrameNotPresent, PageTableLevel::L3)) => {
                    range.step_back();
                    range.skip_l4();
                }

                // if a huge page is found take the error location and use it to find the correct table

                // The indicated level is what was fetched the error actually
                // occurs on the parent
                Err((FrameError::HugeFrame, PageTableLevel::L2)) => {
                    let l3 = self.get_l3_for_addr(page.start_address()).unwrap();
                    let entry = l3[page.p3_index()].clone();
                    if entry.is_unused() {
                        continue;
                    }
                    range.step_back();
                    range.skip_l3();

                    return Some((
                        PageReference {
                            page: page.start_address(),
                            size: PageTableLevel::L3,
                            entry,
                        },
                        range,
                    ));
                }

                Err((FrameError::HugeFrame, PageTableLevel::L1)) => {
                    let l2 = self.get_l3_for_addr(page.start_address()).unwrap();
                    let entry = l2[page.p2_index()].clone();

                    if entry.is_unused() {
                        continue;
                    }

                    range.step_back();
                    range.skip_l2();

                    return Some((
                        PageReference {
                            page: page.start_address(),
                            size: PageTableLevel::L2,
                            entry,
                        },
                        range,
                    ));
                }

                // i think the cpu will #GP before this error happens
                // "this this should never happen huge page reported at l4"
                Err((FrameError::HugeFrame, PageTableLevel::L3)) => panic!(),

                // this should never be returned
                Err((_, PageTableLevel::L4)) => panic!(),
            }
        }

        None
    }

    /// Fetches the PageTable at the suggested level containing the given address
    ///
    /// on error returns a tuple containing the type of error and level at which it
    /// occurred for this fn `3`
    ///
    /// This function is safe however the PageTable returned is mutable and active
    /// and should be treated as unsafe
    fn get_l3_for_addr(
        &self,
        addr: VirtAddr,
    ) -> Result<&mut PageTable, (FrameError, PageTableLevel)> {
        let l4_index = addr.p4_index();
        let entry = &self.l4_table[l4_index];

        return match entry.frame() {
            Ok(frame) => {
                let table_addr = self.offset_base + frame.start_address().as_u64();
                let table = table_addr.as_mut_ptr::<PageTable>();
                unsafe { Ok(&mut *table) }
            }
            Err(err) => Err((err, PageTableLevel::L3)),
        };
    }

    /// see [get_l3_for_addr]
    fn get_l2_for_addr(
        &self,
        addr: VirtAddr,
    ) -> Result<&mut PageTable, (FrameError, PageTableLevel)> {
        let l3_index = addr.p3_index();
        let l3_table;
        match self.get_l3_for_addr(addr) {
            Ok(table) => l3_table = table,
            Err(err) => return Err(err),
        }
        let entry = &l3_table[l3_index];

        return match entry.frame() {
            Ok(frame) => {
                let table_addr = self.offset_base + frame.start_address().as_u64();
                let table = table_addr.as_mut_ptr::<PageTable>();
                unsafe { Ok(&mut *table) }
            }
            Err(err) => Err((err, PageTableLevel::L2)),
        };
    }

    /// see [get_l3_for_addr]
    fn get_l1_for_addr(
        &self,
        addr: VirtAddr,
    ) -> Result<&mut PageTable, (FrameError, PageTableLevel)> {
        let l2_index = addr.p2_index();
        let l2_table;
        match self.get_l2_for_addr(addr) {
            Ok(table) => l2_table = table,
            Err(err) => return Err(err),
        }
        let entry = &l2_table[l2_index];

        return match entry.frame() {
            Ok(frame) => {
                let table_addr = self.offset_base + frame.start_address().as_u64();
                let table = table_addr.as_mut_ptr::<PageTable>();
                unsafe { Ok(&mut *table) }
            }
            Err(err) => Err((err, PageTableLevel::L1)),
        };
    }

    /// Returns number of active PageTables
    pub(super) fn count_tables(&self) -> usize {
        self.count_tables_inner(self.l4_table, PageTableLevel::L4)
    }

    fn count_tables_inner(&self, table: &PageTable, level: PageTableLevel) -> usize {
        let mut count = 0;
        for e in table.iter() {
            if e.flags().contains(PageTableFlags::PRESENT) {
                if e.flags().contains(PageTableFlags::HUGE_PAGE) {
                    continue; // does not contain another table
                } else {
                    let addr = self.offset_base + e.addr().as_u64();

                    let new_table = unsafe { &*addr.as_mut_ptr() };
                    // l1 table does not need to be read because its pages aare not needed
                    if level != PageTableLevel::L2 {
                        count += self.count_tables_inner(new_table, level.dec());
                    }

                    count += 1;
                }
            }
        }

        count
    }

    fn traverse_mut<S: PageSize>(
        &mut self,
        level: PageTableLevel,
        page: Page<S>,
    ) -> Result<&mut PageTable, InternalError> {
        if level == PageTableLevel::L4 {
            return Ok(self.l4_table);
        }
        let page = Page::containing_address(page.start_address());
        let l4 = unsafe { &mut *(self.l4_table as *mut PageTable) };
        self.traverse_inner_mut(PageTableLevel::L4, level, page, l4)
    }

    fn traverse_inner_mut(
        &mut self,
        curr_level: PageTableLevel,
        target_level: PageTableLevel,
        page: Page,
        table: &mut PageTable,
    ) -> Result<&mut PageTable, InternalError> {
        // get addr
        let index = curr_level.get_index(page);
        let entry = table[index].clone();

        // check for table's existence
        if entry.is_unused() {
            return Err(InternalError::PageNotMapped(curr_level.dec()));
        } else if entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            return Err(InternalError::ParentEntryHugePage(curr_level.dec()));
        }

        let addr = entry.addr();
        let table_addr = self.offset_base + addr.as_u64();

        let table = unsafe { &mut *table_addr.as_mut_ptr::<PageTable>() };

        // return or continue

        return if curr_level.dec() == target_level {
            Ok(table)
        } else {
            self.traverse_inner_mut(curr_level.dec(), target_level, page, table)
        };
    }

    fn traverse<S: PageSize>(
        &self,
        level: PageTableLevel,
        page: Page<S>,
    ) -> Result<&PageTable, InternalError> {
        if level == PageTableLevel::L4 {
            return Ok(self.l4_table);
        }
        let page = Page::containing_address(page.start_address());
        self.traverse_inner(PageTableLevel::L4, level, page, self.l4_table)
    }

    fn traverse_inner(
        &self,
        curr_level: PageTableLevel,
        target_level: PageTableLevel,
        page: Page,
        table: &PageTable,
    ) -> Result<&PageTable, InternalError> {
        // get addr
        let index = curr_level.get_index(page);
        let entry = table[index].clone();

        // check for table's existence
        if entry.is_unused() {
            return Err(InternalError::PageNotMapped(curr_level.dec()));
        } else if entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            return Err(InternalError::ParentEntryHugePage(curr_level.dec()));
        }

        let addr = entry.addr();
        let table_addr = self.offset_base + addr.as_u64();

        let table = unsafe { &*table_addr.as_mut_ptr::<PageTable>() };

        // return or continue

        return if curr_level.dec() == target_level {
            Ok(table)
        } else {
            self.traverse_inner(curr_level.dec(), target_level, page, table)
        };
    }

    /// Allocates a new table into memory without mapping it
    fn new_table(&self) -> *mut PageTable {
        let frame: PhysFrame<Size4KiB> = super::allocator::COMBINED_ALLOCATOR.lock().phys_alloc()
            .get()
            .allocate_frame()
            .expect("System ran out of memory");
        // offset addr + new frame as MaybeUninit<PageTable>
        let new_table = unsafe {
            let pt_ref = &mut *((self.offset_base.as_u64() as usize
                + frame.start_address().as_u64() as usize)
                as *mut core::mem::MaybeUninit<PageTable>);
            let tab = pt_ref.write(PageTable::new());
            tab as *mut PageTable
        };
        new_table
    }

    /// Attaches a page table to the currently mapped tree. At the give level and page, The page
    /// will be rounded down for huge pages.
    ///
    /// #Safety
    ///
    /// This fn is unsafe because the reference to `new_table` is assumed to be within the offset
    /// memory. A pointer to a PageTable outside of the offset memory is UB
    unsafe fn attach<S: PageSize>(
        &mut self,
        new_table: *mut PageTable,
        level: PageTableLevel,
        page: Page<S>,
        flags: PageTableFlags,
    ) -> Result<(), InternalError> {
        let page = unsafe { Page::<Size4KiB>::from_start_address_unchecked(page.start_address()) }; // converts Page<S> to PAge<Size4K>

        let frame_addr = new_table as usize - self.offset_base.as_u64() as usize;
        let phys_addr = PhysAddr::new(frame_addr as u64);
        let index = level.inc().get_index(page);

        let table = match self.traverse_mut(level.inc(), page) {
            Ok(table) => table,

            Err(InternalError::PageNotMapped(_)) => {
                // recursively makes new
                let nt = self.new_table();
                self.attach(nt, level.inc(), page, flags).expect("???");
                self.traverse_mut(level.inc(), page).expect("???")
            }

            Err(InternalError::ParentEntryHugePage(l)) => {
                return Err(InternalError::ParentEntryHugePage(l))
            }
            _ => unreachable!(),
        };

        if table[index].is_unused() {
            table[index].set_addr(phys_addr, flags);
            Ok(())
        } else {
            Err(InternalError::PageAlreadyMapped(table[index].addr()))
        }
    }

    /// Returns the page table entry for the requested virtual address.
    pub(crate) fn get_entry(&self, level: PageTableLevel, page: VirtAddr) -> Result<PageTableEntry,GetEntryErr> {
        let page: Page<Size4KiB> = Page::containing_address(page);
        let t = self.traverse(level,page).map_err(|e| {
            match e {
                InternalError::ParentEntryHugePage(_) => GetEntryErr::ParentHugePage,
                InternalError::PageNotMapped(_) => GetEntryErr::NotMapped,
                InternalError::PageAlreadyMapped(_) => unreachable!(), // traverse won't return this
            }
        })?;
        let index = level.get_index(page);
        Ok(*t[index])
    }
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum GetEntryErr {
    /// If this is returned the caller may call [OffsetPageTable::get_entry] again with a higher
    /// level to get the entry.
    ParentHugePage,
    NotMapped,
}

impl Mapper<Size4KiB> for OffsetPageTable {
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
        let table = match self.traverse_mut(PageTableLevel::L1, page) {
            Ok(table) => table,

            Err(InternalError::PageNotMapped(_)) => {
                self.attach(
                    self.new_table(),
                    PageTableLevel::L1,
                    page,
                    parent_table_flags,
                )
                .unwrap(); // Shouldn't panic
                self.traverse_mut(PageTableLevel::L1, page).unwrap() // Shouldn't panic
            }

            Err(InternalError::ParentEntryHugePage(_)) => {
                return Err(MapToError::ParentEntryHugePage)
            }
            _ => unreachable!(),
        };

        let entry = &mut table[PageTableLevel::L1.get_index(page)];
        return if entry.is_unused() {
            entry.set_addr(frame.start_address(), flags);
            Ok(MapperFlush::new(page))
        } else {
            Err(MapToError::PageAlreadyMapped(entry.frame().unwrap())) // Is never huge page
        };
    }

    fn unmap(
        &mut self,
        page: Page<Size4KiB>,
    ) -> Result<(PhysFrame<Size4KiB>, MapperFlush<Size4KiB>), UnmapError> {
        match self.traverse_mut(PageTableLevel::L1, page) {
            Ok(table) => {
                let entry = &mut table[PageTableLevel::L1.get_index(page)];

                if entry.is_unused() {
                    return Err(UnmapError::PageNotMapped);
                }

                let old = core::mem::replace(entry, PageTableEntry::new());
                // P flag is cleared and not restored. shootdown_hint is not required all, faults are genuine
                shootdown(page.into());
                Ok((old.frame().unwrap(), MapperFlush::new(page))) // all errors checked cannot panic
            }

            Err(InternalError::PageNotMapped(_)) => Err(UnmapError::PageNotMapped),
            Err(InternalError::ParentEntryHugePage(_)) => Err(UnmapError::ParentEntryHugePage),
            _ => unreachable!(),
        }
    }

    unsafe fn update_flags(
        &mut self,
        page: Page<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlush<Size4KiB>, FlagUpdateError> {
        match self.traverse_mut(PageTableLevel::L1, page) {
            Ok(table) => {
                shootdown_hint(|| {
                    let entry = &mut table[PageTableLevel::L1.get_index(page)];
                    let mut f = entry.flags();
                    f.set(PageTableFlags::PRESENT, false);
                    entry.set_flags(f);
                    shootdown(page.into());
                    entry.set_flags(flags);
                });
                Ok(MapperFlush::new(page))
            }
            Err(InternalError::PageNotMapped(_)) => Err(FlagUpdateError::PageNotMapped),
            Err(InternalError::ParentEntryHugePage(_)) => Err(FlagUpdateError::ParentEntryHugePage),
            _ => unreachable!(),
        }
    }

    unsafe fn set_flags_p4_entry(
        &mut self,
        page: Page<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        shootdown_hint(|| {
            let entry = &mut self.l4_table[PageTableLevel::L4.get_index(page)];
            if entry.flags().contains(PageTableFlags::PRESENT) {
                let mut f = entry.flags();
                f.set(PageTableFlags::PRESENT,false);
                entry.set_flags(f);
            }
            shootdown(page.into());
            entry.set_flags(flags);
        });
        Ok(MapperFlushAll::new())
    }

    unsafe fn set_flags_p3_entry(
        &mut self,
        page: Page<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = PageTableLevel::L3;
        match self.traverse_mut(LEVEL, page) {
            Ok(table) => {
                shootdown_hint(|| {
                    let entry = &mut table[LEVEL.get_index(page)];
                    if entry.flags().contains(PageTableFlags::PRESENT) {
                        let mut f = entry.flags();
                        f.set(PageTableFlags::PRESENT,false);
                        entry.set_flags(f);
                    }
                    shootdown(page.into());
                    entry.set_flags(flags);
                });
                Ok(MapperFlushAll::new())
            }
            Err(InternalError::PageNotMapped(_)) => Err(FlagUpdateError::PageNotMapped),
            _ => unreachable!(), // huge page is not handled here because the system will fault if it is set
        }
    }

    unsafe fn set_flags_p2_entry(
        &mut self,
        page: Page<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = PageTableLevel::L2;
        match self.traverse_mut(LEVEL, page) {
            Ok(table) => {
                shootdown_hint(|| {
                    let entry = &mut table[LEVEL.get_index(page)];
                    if entry.flags().contains(PageTableFlags::PRESENT) {
                        let mut f = entry.flags();
                        f.set(PageTableFlags::PRESENT,false);
                        entry.set_flags(f);
                    }
                    shootdown(page.into());
                    entry.set_flags(flags);
                });
                Ok(MapperFlushAll::new())
            }
            Err(InternalError::PageNotMapped(_)) => Err(FlagUpdateError::PageNotMapped),
            Err(InternalError::ParentEntryHugePage(_)) => Err(FlagUpdateError::ParentEntryHugePage),
            _ => unreachable!(),
        }
    }

    fn translate_page(&self, page: Page<Size4KiB>) -> Result<PhysFrame<Size4KiB>, TranslateError> {
        match self.traverse(PageTableLevel::L1, page) {
            Ok(table) => {
                let entry = &table[PageTableLevel::L1.get_index(page)];

                if entry.is_unused() {
                    return Err(TranslateError::PageNotMapped);
                }

                Ok(entry.frame().unwrap()) // all errs are checked
            }
            Err(InternalError::PageNotMapped(_)) => Err(TranslateError::PageNotMapped),
            Err(InternalError::ParentEntryHugePage(_)) => Err(TranslateError::ParentEntryHugePage),
            _ => unreachable!(),
        }
    }
}

impl Mapper<Size2MiB> for OffsetPageTable {
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
        const LEVEL: PageTableLevel = PageTableLevel::L2;
        let table = match self.traverse_mut(LEVEL, page) {
            Ok(table) => table,

            Err(InternalError::PageNotMapped(_)) => {
                self.attach(self.new_table(), LEVEL, page, parent_table_flags)
                    .unwrap(); // Shouldn't panic
                self.traverse_mut(PageTableLevel::L2, page).unwrap() // Shouldn't panic
            }

            Err(InternalError::ParentEntryHugePage(_)) => {
                return Err(MapToError::ParentEntryHugePage)
            }
            _ => unreachable!(),
        };

        let entry = &mut table[PageTableLevel::L1.get_index(page)];
        return if entry.is_unused() {
            entry.set_addr(frame.start_address(), flags);
            Ok(MapperFlush::new(page))
        } else {
            unsafe {
                Err(MapToError::PageAlreadyMapped(
                    PhysFrame::from_start_address_unchecked(entry.frame().unwrap().start_address()),
                ))
            } // Is never huge page
        };
    }

    fn unmap(
        &mut self,
        page: Page<Size2MiB>,
    ) -> Result<(PhysFrame<Size2MiB>, MapperFlush<Size2MiB>), UnmapError> {
        const LEVEL: PageTableLevel = PageTableLevel::L2;
        match self.traverse_mut(LEVEL, page) {
            Ok(table) => {
                let entry = &mut table[LEVEL.get_index(page)];

                if entry.is_unused() {
                    return Err(UnmapError::PageNotMapped);
                }

                let old = core::mem::replace(entry, PageTableEntry::new());
                shootdown(ShootdownContent::FullContext);
                Ok((
                    unsafe {
                        PhysFrame::from_start_address_unchecked(
                            old.frame().unwrap().start_address(),
                        )
                    },
                    MapperFlush::new(page),
                )) // all errors checked cannot panic
            }

            Err(InternalError::PageNotMapped(_)) => Err(UnmapError::PageNotMapped),
            Err(InternalError::ParentEntryHugePage(_)) => Err(UnmapError::ParentEntryHugePage),
            _ => unreachable!(),
        }
    }

    unsafe fn update_flags(
        &mut self,
        page: Page<Size2MiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlush<Size2MiB>, FlagUpdateError> {
        const LEVEL: PageTableLevel = PageTableLevel::L2;
        match self.traverse_mut(LEVEL, page) {
            Ok(table) => {
                shootdown_hint(|| {
                    let entry = &mut table[LEVEL.get_index(page)];
                    let mut f = entry.flags();

                    f.set(PageTableFlags::PRESENT, false);
                    entry.set_flags(f);
                    shootdown(ShootdownContent::FullContext);
                    entry.set_flags(flags | PageTableFlags::HUGE_PAGE);
                });
                Ok(MapperFlush::new(page))
            }
            Err(InternalError::PageNotMapped(_)) => Err(FlagUpdateError::PageNotMapped),
            Err(InternalError::ParentEntryHugePage(_)) => Err(FlagUpdateError::ParentEntryHugePage),
            _ => unreachable!(),
        }
    }

    unsafe fn set_flags_p4_entry(
        &mut self,
        page: Page<Size2MiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        shootdown_hint(|| {
            let entry = &mut self.l4_table[PageTableLevel::L4.get_index(page)];
            if entry.flags().contains(PageTableFlags::PRESENT) {
                let mut f = entry.flags();
                f.set(PageTableFlags::PRESENT,false);
                entry.set_flags(f);
            }
            shootdown(page.into());
            entry.set_flags(flags);
        });
        Ok(MapperFlushAll::new())
    }

    unsafe fn set_flags_p3_entry(
        &mut self,
        page: Page<Size2MiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = PageTableLevel::L3;
        match self.traverse_mut(LEVEL, page) {
            Ok(table) => {
                shootdown_hint(|| {
                    let entry = &mut table[LEVEL.get_index(page)];
                    if entry.flags().contains(PageTableFlags::PRESENT) {
                        let mut f = entry.flags();
                        f.set(PageTableFlags::PRESENT,false);
                        entry.set_flags(f);
                    }
                    shootdown(page.into());
                    entry.set_flags(flags);
                });
                Ok(MapperFlushAll::new())
            }
            Err(InternalError::PageNotMapped(_)) => Err(FlagUpdateError::PageNotMapped),
            _ => unreachable!(), // huge page is not handled here because the system will fault if it is set
        }
    }

    unsafe fn set_flags_p2_entry(
        &mut self,
        page: Page<Size2MiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = PageTableLevel::L2;
        match self.traverse_mut(LEVEL, page) {
            Ok(table) => {
                shootdown_hint(|| {
                    let entry = &mut table[LEVEL.get_index(page)];
                    if entry.flags().contains(PageTableFlags::PRESENT) {
                        let mut f = entry.flags();
                        f.set(PageTableFlags::PRESENT,false);
                        entry.set_flags(f);
                    }
                    shootdown(page.into());
                    entry.set_flags(flags);
                });
                Ok(MapperFlushAll::new())
            }
            Err(InternalError::PageNotMapped(_)) => Err(FlagUpdateError::PageNotMapped),
            _ => unreachable!(), // huge page is not handled here because the system will fault if it is set
        }
    }

    fn translate_page(&self, page: Page<Size2MiB>) -> Result<PhysFrame<Size2MiB>, TranslateError> {
        const LEVEL: PageTableLevel = PageTableLevel::L2;
        match self.traverse(LEVEL, page) {
            Ok(table) => {
                let entry = &table[LEVEL.get_index(page)];

                if entry.is_unused() {
                    return Err(TranslateError::PageNotMapped);
                }

                Ok(unsafe {
                    PhysFrame::from_start_address_unchecked(entry.frame().unwrap().start_address())
                }) // all errs are checked
            }
            Err(InternalError::PageNotMapped(_)) => Err(TranslateError::PageNotMapped),
            Err(InternalError::ParentEntryHugePage(_)) => Err(TranslateError::ParentEntryHugePage),
            _ => unreachable!(),
        }
    }
}

impl Mapper<Size1GiB> for OffsetPageTable {
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
        const LEVEL: PageTableLevel = PageTableLevel::L3;
        let table = match self.traverse_mut(LEVEL, page) {
            Ok(table) => table,

            Err(InternalError::PageNotMapped(_)) => {
                self.attach(self.new_table(), LEVEL, page, parent_table_flags)
                    .unwrap(); // Shouldn't panic
                self.traverse_mut(PageTableLevel::L2, page).unwrap() // Shouldn't panic
            }

            Err(InternalError::ParentEntryHugePage(_)) => {
                return Err(MapToError::ParentEntryHugePage)
            }
            _ => unreachable!(),
        };

        let entry = &mut table[PageTableLevel::L1.get_index(page)];
        return if entry.is_unused() {
            entry.set_addr(frame.start_address(), flags);
            Ok(MapperFlush::new(page))
        } else {
            unsafe {
                Err(MapToError::PageAlreadyMapped(
                    PhysFrame::from_start_address_unchecked(entry.frame().unwrap().start_address()),
                ))
            } // Is never huge page
        };
    }

    fn unmap(
        &mut self,
        page: Page<Size1GiB>,
    ) -> Result<(PhysFrame<Size1GiB>, MapperFlush<Size1GiB>), UnmapError> {
        const LEVEL: PageTableLevel = PageTableLevel::L3;
        match self.traverse_mut(LEVEL, page) {
            Ok(table) => {
                let entry = &mut table[LEVEL.get_index(page)];

                if entry.is_unused() {
                    return Err(UnmapError::PageNotMapped);
                }

                let old = core::mem::replace(entry, PageTableEntry::new());
                shootdown(page.into());
                Ok((
                    unsafe {
                        PhysFrame::from_start_address_unchecked(
                            old.frame().unwrap().start_address(),
                        )
                    },
                    MapperFlush::new(page),
                )) // all errors checked cannot panic
            }

            Err(InternalError::PageNotMapped(_)) => Err(UnmapError::PageNotMapped),
            Err(InternalError::ParentEntryHugePage(_)) => Err(UnmapError::ParentEntryHugePage),
            _ => unreachable!(),
        }
    }

    unsafe fn update_flags(
        &mut self,
        page: Page<Size1GiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlush<Size1GiB>, FlagUpdateError> {
        const LEVEL: PageTableLevel = PageTableLevel::L3;
        match self.traverse_mut(LEVEL, page) {
            Ok(table) => {
                shootdown_hint(|| {
                    let entry = &mut table[LEVEL.get_index(page)];
                    let mut f = entry.flags();

                    f.set(PageTableFlags::PRESENT, false);
                    entry.set_flags(f);
                    shootdown(ShootdownContent::FullContext);
                    entry.set_flags(flags | PageTableFlags::HUGE_PAGE);
                });
                Ok(MapperFlush::new(page))
            }
            Err(InternalError::PageNotMapped(_)) => Err(FlagUpdateError::PageNotMapped),
            Err(InternalError::ParentEntryHugePage(_)) => Err(FlagUpdateError::ParentEntryHugePage),
            _ => unreachable!(),
        }
    }

    unsafe fn set_flags_p4_entry(
        &mut self,
        page: Page<Size1GiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        shootdown_hint(|| {
            let entry = &mut self.l4_table[PageTableLevel::L4.get_index(page)];
            if entry.flags().contains(PageTableFlags::PRESENT) {
                let mut f = entry.flags();
                f.set(PageTableFlags::PRESENT,false);
                entry.set_flags(f);
            }
            shootdown(page.into());
            entry.set_flags(flags);
        });
        Ok(MapperFlushAll::new())
    }

    unsafe fn set_flags_p3_entry(
        &mut self,
        page: Page<Size1GiB>,
        flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        const LEVEL: PageTableLevel = PageTableLevel::L3;
        match self.traverse_mut(LEVEL, page) {
            Ok(table) => {
                shootdown_hint(|| {
                    let entry = &mut table[LEVEL.get_index(page)];
                    if entry.flags().contains(PageTableFlags::PRESENT) {
                        let mut f = entry.flags();
                        f.set(PageTableFlags::PRESENT,false);
                        entry.set_flags(f);
                    }
                    shootdown(page.into());
                    entry.set_flags(flags);
                });
                Ok(MapperFlushAll::new())
            }
            Err(InternalError::PageNotMapped(_)) => Err(FlagUpdateError::PageNotMapped),
            _ => unreachable!(), // huge page is not handled here because the system will fault if it is set
        }
    }

    unsafe fn set_flags_p2_entry(
        &mut self,
        _page: Page<Size1GiB>,
        _flags: PageTableFlags,
    ) -> Result<MapperFlushAll, FlagUpdateError> {
        Err(FlagUpdateError::ParentEntryHugePage)
    }

    fn translate_page(&self, page: Page<Size1GiB>) -> Result<PhysFrame<Size1GiB>, TranslateError> {
        const LEVEL: PageTableLevel = PageTableLevel::L3;
        match self.traverse(LEVEL, page) {
            Ok(table) => {
                let entry = &table[LEVEL.get_index(page)];

                if entry.is_unused() {
                    return Err(TranslateError::PageNotMapped);
                }

                Ok(unsafe {
                    PhysFrame::from_start_address_unchecked(entry.frame().unwrap().start_address())
                }) // all errs are checked
            }
            Err(InternalError::PageNotMapped(_)) => Err(TranslateError::PageNotMapped),
            Err(InternalError::ParentEntryHugePage(_)) => Err(TranslateError::ParentEntryHugePage),
            _ => unreachable!(),
        }
    }
}
// todo move to mem
#[derive(Debug)]
pub(super) struct PageReference {
    pub page: VirtAddr,
    pub size: PageTableLevel,
    pub entry: PageTableEntry,
}
