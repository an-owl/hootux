use core::alloc::{Allocator, Layout};
use core::mem;
use acpi::{AcpiHandler, PhysicalMapping};
use crate::allocator::mmio_bump_alloc::MmioAlloc;

#[derive(Copy, Clone)]
pub struct AcpiGrabber;

impl AcpiHandler for AcpiGrabber {
    unsafe fn map_physical_region<T>(&self, physical_address: usize, size: usize) -> PhysicalMapping<Self, T> {
        let alloc = MmioAlloc::new_from_usize(physical_address);
        let region = alloc.allocate(
            Layout::from_size_align(
                size,
                mem::align_of::<T>())
                .expect("Failed to create layout for ACPI object"))
            .expect("ACPI object failed to allocate");

        let addr = region.cast::<T>();

        let mapped_length = {
            const MASK: usize = 4096-1;
            let start = (physical_address | MASK) + 1;
            let end = (physical_address + size) & (!MASK - 1);

            end - start
        };


        unsafe {
            PhysicalMapping::new(
                physical_address,
                addr,
                size,
                mapped_length,
                self.clone()

            )
        }

    }

    fn unmap_physical_region<T>(region: &PhysicalMapping<Self, T>) {
        let alloc = MmioAlloc::new_from_usize(region.physical_start());
        let start = region.virtual_start();

        unsafe {
            alloc.deallocate(
                start.cast(),
                Layout::from_size_align(
                    region.region_length(),
                    mem::align_of::<T>()).unwrap()) // should not panic. blame acpi if it does
        }
    }
}