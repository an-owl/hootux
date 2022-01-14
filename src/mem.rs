use x86_64::{structures::paging::PageTable,VirtAddr};

pub unsafe fn active_l4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable{
    use x86_64::registers::control::Cr3;
    let (l4,_) = Cr3::read();

    let phys = l4.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    &mut *page_table_ptr

}