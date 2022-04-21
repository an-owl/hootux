use core::marker::PhantomPinned;
use core::ops::{Deref, DerefMut};
use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::{Mapper, Page, Size4KiB};

pub static mut APIC_REGION: ApicMMIO = ApicMMIO::new();

// highest value is +0x3f0 /4 (size of u32) is 252
const APIC_MMIO_SIZE: usize = 252;

#[repr(align(4096))]
/// This struct is to allow the creation of a pinned array usable for an
/// xAPIC memory mapped io region
///
///
pub struct ApicMMIO{
    data: [u32; APIC_MMIO_SIZE],
    _phantom_pin: core::marker::PhantomPinned
}

impl ApicMMIO{
    /// create new ApicMMIO
    pub const fn new() -> Self{
        Self{ data:[0u32; APIC_MMIO_SIZE], _phantom_pin: PhantomPinned{} }
    }

    /// creates new ApicMMIO at physical address `p`
    ///
    /// this is not yet implemented because the current allocator needs to be rebuilt
    pub fn new_at(p: PhysAddr, mapper: &impl Mapper<Size4KiB>) -> Self{
        todo!()
    }

    pub fn physical_location(&self, mapper: &impl Mapper<Size4KiB>) -> PhysAddr{
        let addr = mapper.translate_page(Page::containing_address(VirtAddr::from_ptr(&self.data))).unwrap().start_address();
        assert_eq!(addr.as_u64() % 4096, VirtAddr::from_ptr(&self.data).as_u64() % 4096, "ApicMMIO data Misaligned" );
        addr
    }
}

impl Deref for ApicMMIO{
    type Target = [u32; APIC_MMIO_SIZE];

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}
impl DerefMut for ApicMMIO{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}
