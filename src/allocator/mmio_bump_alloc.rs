use core::alloc::{Allocator, AllocError, Layout};
use core::ptr::NonNull;
use log::{debug, trace};
use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::structures::paging::page::PageRange;
use crate::mem;
use crate::mem::DummyFrameAlloc;

#[derive(Copy, Clone)]
/// Wrapper for heap manager. Implements Allocator but physical a physical address must be provided
pub struct MmioAlloc{
    physical_addr: PhysAddr
}

impl MmioAlloc{
    pub const fn new(phys_addr: PhysAddr) -> Self {
        Self{physical_addr: phys_addr}
    }
    pub const fn new_from_usize(phys_addr: usize) -> Self {
        Self{ physical_addr: PhysAddr::new(phys_addr as u64) }
    }

    /// Preforms uncorrected alloc. Only returns address aligned to 0x1000.
    /// This fn is required because of a separate allocator returning [acpi::PhysicalMapping]
    fn alloc_inner(&self, layout: Layout) -> Result<*mut u8, AllocError> {
        // gen internal layout
        let  phys_end: PhysAddr = (self.physical_addr + layout.size()) - 1u64; // gets the last byte of `layout` not the byte after, required for maths afterwards
        let  phys_start = self.physical_addr;

        let size = phys_end.align_up(mem::PAGE_SIZE as u64) - phys_start.align_down(mem::PAGE_SIZE as u64); // something has gone very wrong if this is less than 4K
        assert_eq!(size % mem::PAGE_SIZE as u64, 0, "your maths is shit");

        let internal_layout = unsafe { Layout::from_size_align_unchecked(size as usize, mem::PAGE_SIZE) };

        //call alloc
        let alloc_out = crate::kernel_statics::fetch_local().mmio_heap_man.allocate(internal_layout)?;

        //map region
        let base_page =  Page::<Size4KiB>::containing_address(VirtAddr::from_ptr(alloc_out as *const u8 ));
        let end_page =  Page::containing_address(base_page.start_address() + size);

        let range = PageRange{start: base_page, end: end_page};
        let phys_addr = self.physical_addr;



        for page in range {
            unsafe {
                crate::kernel_statics::fetch_local().page_table_tree.map_to(
                    page,
                    PhysFrame::containing_address(phys_addr),
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
                    &mut DummyFrameAlloc
                ).unwrap().flush();
            }
        }

        trace!("mapped: {:x}..={:x}",base_page.start_address().as_u64(),end_page.start_address().as_u64());

        Ok(alloc_out)
    }
}

unsafe impl Allocator for MmioAlloc{

    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {

        let aligned_addr = self.alloc_inner(layout)?;
        let offset_addr = aligned_addr as usize + (self.physical_addr.as_u64() as usize & 0xfff);

        trace!("mapped {:x} to {:x}", self.physical_addr.as_u64(), offset_addr);

        let allocated_region = unsafe {
            let arr = core::slice::from_raw_parts_mut(offset_addr as *mut u8, layout.size());
            NonNull::new_unchecked(arr)
        }; // unchecked because the system is probably already broken if this is null
        Ok(allocated_region)


    }

    fn allocate_zeroed(&self, _layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        panic!("Tried to MmioAlloc zeroed");
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        crate::kernel_statics::fetch_local().mmio_heap_man.deallocate(layout);

        let start_page =  Page::<Size4KiB>::containing_address(VirtAddr::from_ptr(ptr.as_ptr()));
        let end_page = Page::containing_address( start_page.start_address() + layout.size() );

        let pages = PageRange{start: start_page, end: end_page};

        for page in pages {
            crate::kernel_statics::fetch_local().page_table_tree.unmap(page).unwrap().1.flush();
        }
    }
}

#[derive(Debug)]
/// Heap manager for MmioAlloc
/// start and end should remain constant after `init()` is called
/// Referenced memory should *not* be mapped by default and should be mapped on alloc
///
/// MmioBumpHeap does not directly allocate memory as such all contained methods are marked safe.
/// MmioBumpHEap is designed to be wrapped by MmioAlloc so allocate and deallocate functions
/// should not be called manually
pub(crate) struct MmioBumpHeap{
    start: VirtAddr,
    head: VirtAddr,
    end: VirtAddr,
    count: usize,
}

impl MmioBumpHeap{
    pub const HEAP_START: usize = 0xFF0000000000; // start of l4 [510]
    pub const HEAP_SIZE: usize = 0x200000; // 2M
    ///Creates a new instance of MmioBumpHeap
    pub(crate) const fn new() -> Self {
        Self{
            start: VirtAddr::zero(),
            head: VirtAddr::zero(),
            end: VirtAddr::zero(),
            count: 0
        }
    }

    /// Initializes self to the given memory-range
    ///
    /// This function will panic if `start > end`
    pub(crate) fn init(&mut self, start: VirtAddr, end: VirtAddr) {
        assert!(start < end);
        self.head = start;
        self.start = start;
        self.end = end;
    }

    /// Allocates address range for layout.
    /// layout should be aligned to 4096. Failing to do so may cause undefined behaviour
    /// returns raw pointer because &\[u8\] would cause undefined behaviour
    fn allocate(&mut self, layout: Layout) -> Result<*mut u8, AllocError> {

        self.count += 1;
        if self.head + layout.size() > self.end {
            return Err(AllocError)
        }

        self.head = self.head.align_up(layout.align() as u64);

        let ret = self.head.as_mut_ptr();


        self.head += layout.size();

        Ok(ret)
    }

    /// "deallocates space for layout"
    /// this only actually decrements the internal counter when it reaches 0 head is reset
    fn deallocate(&mut self, _layout: Layout) {
        self.count -= 1;

        if self.count == 0 {
            debug!("MmioBumpHeap: Reset to count=0");
            self.head = self.start;
        }
    }


}