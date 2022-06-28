use crate::mem::{page_table_tree::PageTableBranch, PageTableLevel};
use acpi::{AcpiHandler, PhysicalMapping};
use core::cell::Cell;
use core::ptr::NonNull;
use linked_list_allocator::align_down;
use x86_64::structures::paging::{PageTableIndex, PhysFrame};
use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::mapper::MapperFlushAll;

#[allow(dead_code)]
fn init(){
    todo!();
}

// todo do this properly once an on demand page allocator is made
#[derive(Clone)]
pub struct AcpiGrabber<'a> {
    page_table: &'a Cell<PageTableBranch>,
    virt_base: VirtAddr,
}

impl<'a> AcpiGrabber<'a> {
    const PAGE_SIZE: usize = 4096;
    pub fn new(page_table: &'a mut PageTableBranch, virt_base: VirtAddr) -> Self {
        match page_table.level() {
            PageTableLevel::L1 => {}
            p => panic!("tried to use {:?} for AcpiGrabber", p),
        }

        let page_table = Cell::from_mut(page_table);
        Self {
            page_table,
            virt_base,
        }
    }

    /// finds a free region of `frame_count` within th local page table
    fn find_free(&self, frame_count: usize) -> Option<usize> {
        let mut base = None;
        for (i, e) in (unsafe { &*self.page_table.as_ptr() }).get_page().iter().enumerate() {
            if e.is_unused() {
                if let None = base {
                    // sets base if it is not set
                    base = Some(i)
                } else {
                    // checks if its found enough space
                    if base.unwrap() + frame_count == i {
                        return base;
                    }
                }
            } else {
                //reset base to None if the found region isn't big enough
                base = None
            }
        }
        None
    }
}

impl<'a> AcpiHandler for AcpiGrabber<'a> {
    #[allow(unused_unsafe)]
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> PhysicalMapping<Self, T> {
        // gather numbers
        let new_phy = align_down(physical_address, 4096);
        let offset = physical_address - new_phy;
        let frame_count = ((offset + size) / Self::PAGE_SIZE) + 1;

        // allocate pages
        let start_page = self
            .find_free(frame_count)
            .expect("not enough space for Acpi handler");
        let mut p_addr = new_phy;

        for p in start_page..start_page + frame_count {
            unsafe { &mut *self.page_table.as_ptr() }
                .allocate_frame(PageTableIndex::new(p as u16), PhysFrame::containing_address(PhysAddr::new(p_addr as u64)));
            MapperFlushAll::new().flush_all();
            p_addr += Self::PAGE_SIZE
        }

        let ptr = offset + self.virt_base.as_u64() as usize + (start_page * Self::PAGE_SIZE);
        let ptr = ptr as *mut T;

        PhysicalMapping::new(
            physical_address,            // potentially unaligned
            NonNull::new_unchecked(ptr), // ptr is never null
            size,
            frame_count * Self::PAGE_SIZE,
            self.clone(),
        )
    }

    fn unmap_physical_region<T>(region: &PhysicalMapping<Self, T>) {
        let page_count = (region.mapped_length() / Self::PAGE_SIZE) as u16;

        let table_index = {
            let ptr = u16::from(VirtAddr::from_ptr(region.virtual_start().as_ptr()).p1_index());
            ptr
        };

        for i in table_index..table_index + page_count {
            unsafe { &mut *region.handler().page_table.as_ptr() }.drop_child(PageTableIndex::new(i)) // this shouldn't panic
        }
    }
}
