use x86_64::{structures::paging::PageTable,VirtAddr,PhysAddr};
use x86_64::structures::paging::{FrameAllocator, Mapper, OffsetPageTable, Page, PhysFrame, Size2MiB, Size4KiB};

unsafe fn active_l4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable{
    use x86_64::registers::control::Cr3;
    let (l4,_) = Cr3::read();

    let phys = l4.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    &mut *page_table_ptr

}

///gets a physical address from a virtual one, or returns `None` bif the address is not valid
pub unsafe fn translate_addr(addr: VirtAddr, offset: VirtAddr) -> Option<PhysAddr> {

    translate_addr_inner(addr,offset)
}
//fuck does this do?

fn translate_addr_inner(addr: VirtAddr, offset: VirtAddr) -> Option<PhysAddr> {
    use x86_64::structures::paging::page_table::FrameError;
    use x86_64::registers::control::Cr3;

    let (l4,_) = Cr3::read();

    //get address index of all frames
    let table_indices = [
        addr.p4_index(), addr.p3_index(),addr.p2_index(), addr.p1_index()
    ];

    //start at l4
    let mut frame = l4;


    for &index in &table_indices{
        //get current table
        let virt = offset + frame.start_address().as_u64();
        let table_ptr: *const PageTable = virt.as_ptr();
        let table = unsafe { &*table_ptr};

        //look through table for next address frame
        //frame will contain child table or physical frame for `addr`
        let entry = &table[index];
        frame = match entry.frame(){
            Ok(frame) => frame,
            Err(FrameError::FrameNotPresent) => return None,
            Err(FrameError::HugeFrame) => panic!("Cant lookup huge pages")
        };

    }

    Some(frame.start_address() + u64::from(addr.page_offset()))
}

pub unsafe fn init(offset: VirtAddr) -> OffsetPageTable<'static> {
    let l4 = active_l4_table(offset);
    OffsetPageTable::new(l4,offset)
}

pub fn create_example_mapping(
    page: Page,
    mapper: &mut OffsetPageTable,
    allocator: &mut impl FrameAllocator<Size4KiB>
){
    use x86_64::structures::paging::PageTableFlags as Flags;

    let frame = PhysFrame::containing_address(PhysAddr::new(0xb8000));
    let f = Flags::PRESENT | Flags::WRITABLE;

    let map_to_result = unsafe{
        mapper.map_to(page,frame,f,allocator)
    };

    map_to_result.expect("failed to map").flush();
}

pub struct EmptyFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for EmptyFrameAllocator{
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        None
    }
}