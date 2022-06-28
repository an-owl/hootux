use crate::mem::{PageIterator, PageSizeLevel};
use super::PageTableLevel;
use alloc::vec::Vec;
use x86_64::structures::paging::page::PageRangeInclusive;
use x86_64::structures::paging::page_table::{FrameError, PageTableEntry};
use x86_64::structures::paging::{
    Page, PageSize, PageTable, PageTableFlags, PhysFrame, Size2MiB, Size4KiB,
};
use x86_64::VirtAddr;

pub(super) struct OffsetPageTable {
    offset_base: VirtAddr,
    l4_table: &'static mut PageTable,
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

    /// Returns base address
    pub(super) fn get_base_addr(&self) -> VirtAddr {
        self.offset_base
    }

    /// Returns all mapped pages and their frames from `start` to `end`
    pub(super) fn get_allocated_frames_within(&self, mut range: PageIterator) -> Option<(PageReference, PageIterator)> {

        // this spaghetti needs some sauce but this is the best i can do
        // each page is iterated in order to get its frame entry
        while let Some(page) = range.next() {
            // each page in the loop represents 4k if a huge page is found this must handled
            // this is done by skipping each page in the loop

            // get l1 table of page so the table entry may be looked up
            match self.get_l1_for_addr(page.start_address()) {
                Ok(l1) => {

                    let entry = l1[page.p1_index()].clone();
                    if entry.is_unused(){
                        continue
                    }

                    return Some((PageReference{page: page.start_address(), size: PageTableLevel::L1, entry }, range))
                }

                // if no frame is found then continue because it does not need to be copied
                // must skip appropriately
                Err((FrameError::FrameNotPresent, PageTableLevel::L1)) => { range.step_back(); range.skip_l2(); },
                Err((FrameError::FrameNotPresent, PageTableLevel::L2)) => { range.step_back(); range.skip_l3(); },
                Err((FrameError::FrameNotPresent, PageTableLevel::L3)) => { range.step_back(); range.skip_l4(); },

                // if a huge page is found take the error location and use it to find the correct table

                // The indicated level is what was fetched the error actually
                // occurs on the parent


                Err((FrameError::HugeFrame, PageTableLevel::L2)) =>  {
                    let l3 = self.get_l3_for_addr(page.start_address()).unwrap();
                    let entry = l3[page.p3_index()].clone();
                    if entry.is_unused(){
                        continue
                    }
                    range.step_back();
                    range.skip_l3();

                    return Some((PageReference{page: page.start_address(), size: PageTableLevel::L3, entry }, range))
                }

                Err((FrameError::HugeFrame, PageTableLevel::L1)) => {
                    let l2 = self.get_l3_for_addr(page.start_address()).unwrap();
                            let entry = l2[page.p2_index()].clone();

                            if entry.is_unused(){
                                continue
                            }

                            range.step_back();
                            range.skip_l2();

                            return Some((PageReference{page: page.start_address(), size: PageTableLevel::L2, entry }, range))
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

    /// Returns the PageTable at the given frame
    ///
    /// This function is unsafe because the caller must ensure that
    /// `frame` contains a valid PageTable
    unsafe fn get_table_from_frame(&self, frame: PhysFrame) -> &'static PageTable {
        &*(self.offset_base + frame.start_address().as_u64()).as_mut_ptr::<PageTable>()
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
        let mut count= 0;
        for e in table.iter() {
            if e.flags().contains(PageTableFlags::PRESENT){
                if e.flags().contains(PageTableFlags::HUGE_PAGE){
                    continue // does not contain another table
                } else {

                    let addr = self.offset_base + e.addr().as_u64();

                    let new_table = unsafe { &*addr.as_mut_ptr() };
                    // l1 table does not need to be read because its pages aare not needed
                    if level != PageTableLevel::L2{
                        count += self.count_tables_inner(new_table, level.dec());
                    }

                    count += 1;
                }
            }
        }

        count
    }
}

// todo move to mem
#[derive(Debug)]
pub(super) struct PageReference {
    pub page: VirtAddr,
    pub size: super::PageTableLevel,
    pub entry: PageTableEntry,
}