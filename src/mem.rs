use x86_64::{structures::paging::PageTable,VirtAddr,PhysAddr};
use x86_64::structures::paging::{FrameAllocator, OffsetPageTable, PhysFrame, Size4KiB};
use bootloader::bootinfo::{MemoryMap, MemoryRegionType};

pub struct BootInfoFrameAllocator{
    memory_map: &'static MemoryMap,
    next: usize,
}

impl BootInfoFrameAllocator{

    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
        }
    }
    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        let regions = self.memory_map.iter();
        let usable_regions = regions
            .filter(|r| r.region_type == MemoryRegionType::Usable);

        let addr_ranges = usable_regions
            .map(|r| r.range.start_addr()..r.range.end_addr());

        let frame_addresses = addr_ranges.flat_map(|r| r.step_by(4096));

        frame_addresses.map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator{
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}

unsafe fn active_l4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable{
    use x86_64::registers::control::Cr3;
    let (l4,_) = Cr3::read();

    let phys = l4.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    &mut *page_table_ptr

}

pub unsafe fn init(offset: VirtAddr) -> OffsetPageTable<'static> {
    let l4 = active_l4_table(offset);
    OffsetPageTable::new(l4,offset)
}