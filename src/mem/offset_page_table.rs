use alloc::vec::Vec;
use x86_64::structures::paging::{Page, PageSize, PageTable, PageTableFlags, PhysFrame, Size2MiB, Size4KiB};
use x86_64::structures::paging::page::PageRangeInclusive;
use x86_64::structures::paging::page_table::{FrameError, PageTableEntry};
use x86_64::VirtAddr;
use crate::mem::PageSizeLevel;

struct OffsetPageTable{
    offset_base: VirtAddr,
    l4_table: &'static mut PageTable
}

impl OffsetPageTable{

    /// Initializes OffsetPageTable
    ///
    /// This function is unsafe because the caller must ensure that the
    /// given `offset_base` is correct
    unsafe fn new(offset_base: VirtAddr) -> Self{
        let l4_frame = x86_64::registers::control::Cr3::read().0;
        let l4_table = &mut *(offset_base + l4_frame.start_address().as_u64()).as_mut_ptr::<PageTable>();

        Self{offset_base,l4_table}
    }

    /// Returns base address
    fn get_base_addr(&self) -> VirtAddr{
        self.offset_base
    }

    /// Returns all mapped pages and their frames from `start` to `end`
    fn get_allocated_frames_within(&self, start: Page, end: Page) -> Vec<PageReference> {
        let mut page_ref_list = Vec::new();
        let range = PageRangeInclusive{start, end};

        let mut skip = 0;

        // this spaghetti needs some sauce but this is the best i can do
        // each page is iterated in order to get its frame entry
        for page in range{

            // each page in the loop represents 4k if a huge page is found this must handled
            // this is done by skipping each page in the loop
            if skip > 0 {
                skip -= 1;
                continue
            }

            // get l1 table of page so the table entry may be looked up
            match self.get_l1_for_addr(page.start_address()){
                Ok(l1) => {
                    let entry = l1[page.p1_index()].clone();
                    page_ref_list.push(PageReference{
                        page: page.start_address(), // addresses are used because of Page<T> where T is stupid
                        size: PageSizeLevel::L1,
                        entry
                    })
                }

                // if no frame is found then continue because it does not need to be copied
                Err((FrameError::FrameNotPresent,_)) => {
                    continue
                }

                // if a huge page is found take the error location and use it to find the correct table
                Err((FrameError::HugeFrame,level)) => {
                    match level{
                        // the levels indicated here are triggered by the table above them so they are 1 level lower than you'd expect
                        FrameErrorLevel::L3 => {
                            // i think the cpu will #GP before this error happens
                            panic! ("this this should never happen\nhuge page reported at l4")
                        }

                        FrameErrorLevel::L2 => {
                            let l3 = self.get_l3_for_addr(page.start_address()).unwrap();
                            let entry = l3[page.p3_index()].clone();

                            page_ref_list.push(PageReference{
                                page: page.start_address(),
                                size: PageSizeLevel::L3,
                                entry
                            });

                            skip = PageSizeLevel::L3.num_4k_pages();

                        }

                        FrameErrorLevel::L1 => {
                            let l2 = self.get_l3_for_addr(page.start_address()).unwrap();
                            let entry = l2[page.p2_index()].clone();

                            page_ref_list.push(PageReference{
                                page: page.start_address(),
                                size: PageSizeLevel::L2,
                                entry
                            });

                            skip = PageSizeLevel::L2.num_4k_pages();

                        }
                    }
                }
            }
        }

        page_ref_list
    }

    /// Returns the PageTable at the given frame
    ///
    /// This function is unsafe because the caller must ensure that
    /// `frame` contains a valid PageTable
    unsafe fn get_table_from_frame(&self, frame: PhysFrame) -> &'static PageTable{
        &*(self.offset_base + frame.start_address().as_u64()).as_mut_ptr::<PageTable>()
    }

    /// Fetches the PageTable at the suggested level containing the given address
    ///
    /// on error returns a tuple containing the type of error and level at which it
    /// occurred for this fn `3`
    ///
    /// This function is safe however the PageTable returned is mutable and active
    /// and should be treated as unsafe
    fn get_l3_for_addr(&self, addr: VirtAddr) -> Result<&mut PageTable,(FrameError, FrameErrorLevel)>{
        let l4_index = addr.p4_index();
        let entry = &self.l4_table[l4_index];

        return match entry.frame() {
            Ok(frame) => {
                let table_addr = self.offset_base + frame.start_address().as_u64();
                let table = table_addr.as_mut_ptr::<PageTable>();
                unsafe { Ok(&mut *table) }
            }
            Err(err) => {
                Err((err,FrameErrorLevel::L3))
            }
        }
    }

    /// see [get_l3_for_addr]
    fn get_l2_for_addr(&self, addr: VirtAddr) -> Result<&mut PageTable, (FrameError,FrameErrorLevel)> {
        let l3_index = addr.p3_index();
        let l3_table;
        match self.get_l3_for_addr(addr){
            Ok(table) => {
                l3_table = table
            }
            Err(err) => return Err(err),
        }
        let entry = &l3_table[l3_index];

        return match entry.frame() {
            Ok(frame) => {
                let table_addr = self.offset_base + frame.start_address().as_u64();
                let table = table_addr.as_mut_ptr::<PageTable>();
                unsafe { Ok(&mut *table) }
            }
            Err(err) => {
                Err((err,FrameErrorLevel::L2))
            }
        }
    }

    /// see [get_l3_for_addr]
    fn get_l1_for_addr(&self, addr: VirtAddr) -> Result<&mut PageTable, (FrameError, FrameErrorLevel)> {
        let l2_index = addr.p2_index();
        let l2_table;
        match self.get_l2_for_addr(addr){
            Ok(table) => {
                l2_table = table
            }
            Err(err) => return Err(err),
        }
        let entry = &l2_table[l2_index];

        return match entry.frame() {
            Ok(frame) => {
                let table_addr = self.offset_base + frame.start_address().as_u64();
                let table = table_addr.as_mut_ptr::<PageTable>();
                unsafe { Ok(&mut *table) }
            }
            Err(err) => {
                Err((err,FrameErrorLevel::L1))
            }
        }
    }
}

struct PageReference{
    page: VirtAddr,
    size: super::PageSizeLevel,
    entry: PageTableEntry
}

#[derive(Debug)]
enum FrameErrorLevel{
    L3,
    L2,
    L1,
}