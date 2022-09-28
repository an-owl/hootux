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

pub (crate) mod data_access {
    //! This module if for allowing access to Unsized system data without using `&dyn` that may or
    //! may not be accessed through memory. This is done using a type storing the bus address and size
    //! of the data. [DataAccessType] is provided to allow a universal access to needed data.


    use core::alloc::{Allocator, Layout};
    use acpi::platform::address::{AccessSize, AddressSpace, GenericAddress};
    use x86_64::VirtAddr;
    use crate::allocator::mmio_bump_alloc::MmioAlloc;

    /// Contains Data read using [DataAccess] contains u64 along with the length of the data within
    ///
    /// Implements TryInto<u8..64> and TryInto<i8..64> to allow for simple access to the stored data.
    /// These only return Err when `self.size` is less than the size requested
    pub(crate) struct AcpiData {
        size: DataSize,
        data: u64,
    }

    #[allow(dead_code)]
    impl AcpiData {
        /// returns number of bytes used in data
        pub fn size(&self) -> u8 {
            self.size.into()
        }

        /// returns raw data contained within self
        fn data(&self) -> u64 {
            self.data
        }
    }

    impl TryInto<u8> for AcpiData {
        type Error = DataSize;

        fn try_into(self) -> Result<u8, Self::Error> {
            return if self.size >= DataSize::Byte {
                Ok(self.data as u8)
            } else { Err(self.size) }
        }
    }

    impl TryInto<i8> for AcpiData {
        type Error = DataSize;

        fn try_into(self) -> Result<i8, Self::Error> {
            return if self.size >= DataSize::Byte {
                Ok(self.data as i8)
            } else { Err(self.size) }
        }
    }

    impl TryInto<u16> for AcpiData {
        type Error = DataSize;

        fn try_into(self) -> Result<u16, Self::Error> {
            return if self.size >= DataSize::Word {
                Ok(self.data as u16)
            } else { Err(self.size) }
        }
    }

    impl TryInto<i16> for AcpiData {
        type Error = DataSize;

        fn try_into(self) -> Result<i16, Self::Error> {
            return if self.size >= DataSize::Word {
                Ok(self.data as i16)
            } else { Err(self.size) }
        }
    }

    impl TryInto<u32> for AcpiData {
        type Error = DataSize;

        fn try_into(self) -> Result<u32, Self::Error> {
            return if self.size >= DataSize::DWord {
                Ok(self.data as u32)
            } else { Err(self.size) }
        }
    }

    impl TryInto<i32> for AcpiData {
        type Error = DataSize;

        fn try_into(self) -> Result<i32, Self::Error> {
            return if self.size >= DataSize::DWord {
                Ok(self.data as i32)
            } else { Err(self.size) }
        }
    }

    impl TryInto<u64> for AcpiData {
        type Error = DataSize;

        fn try_into(self) -> Result<u64, Self::Error> {
            return if self.size >= DataSize::QWord {
                Ok(self.data as u64)
            } else { Err(self.size) }
        }
    }

    impl TryInto<i64> for AcpiData {
        type Error = DataSize;

        fn try_into(self) -> Result<i64, Self::Error> {
            return if self.size >= DataSize::QWord {
                Ok(self.data as i64)
            } else { Err(self.size) }
        }
    }


    #[derive(Debug, Copy, Clone, PartialOrd, PartialEq)]
    pub(crate) enum DataSize {
        Byte,
        Word,
        DWord,
        QWord,
    }

    impl Into<u8> for DataSize {
        fn into(self) -> u8 {
            match self {
                DataSize::Byte => 1,
                DataSize::Word => 2,
                DataSize::DWord => 4,
                DataSize::QWord => 8,
            }
        }
    }

    impl From<AccessSize> for DataSize {
        fn from(o: AccessSize) -> Self {
            match o {
                AccessSize::Undefined => panic!("Undefined access size"),
                AccessSize::ByteAccess => Self::Byte,
                AccessSize::WordAccess => Self::Word,
                AccessSize::DWordAccess => Self::DWord,
                AccessSize::QWordAccess => Self::QWord,
            }
        }
    }


    /// Trait for accessing Data behind an [acpi::platform::address::GenericAddress]
    pub(crate) trait DataAccess
        where Self: Sized
    {
        fn new(addr: usize, size: DataSize) -> Self;
        unsafe fn read(&self) -> AcpiData;
        unsafe fn write(&mut self, value: u64);
    }

    pub(crate) struct PortAccess {
        data_size: DataSize,
        port_addr: u16,
    }

    impl DataAccess for PortAccess {
        fn new(addr: usize, size: DataSize) -> Self {
            Self {
                data_size: size,
                port_addr: addr as u16, // values above u16::MAX are erroneous
            }
        }

        unsafe fn read(&self) -> AcpiData {
            use x86_64::instructions::port::Port;
            let num = match self.data_size {
                DataSize::Byte => {
                    let data: u8 = Port::new(self.port_addr).read();
                    data as u64
                }
                DataSize::Word => {
                    let data: u16 = Port::new(self.port_addr).read();
                    data as u64
                }
                DataSize::DWord => {
                    let data: u32 = Port::new(self.port_addr).read();
                    data as u64
                }
                DataSize::QWord => {
                    unreachable!();
                }
            };

            AcpiData {
                size: self.data_size,
                data: num,
            }
        }

        unsafe fn write(&mut self, value: u64) {
            use x86_64::instructions::port::Port;
            match self.data_size {
                DataSize::Byte => {
                    Port::new(self.port_addr).write(value as u8);
                }
                DataSize::Word => {
                    Port::new(self.port_addr).write(value as u16);
                }
                DataSize::DWord => {
                    Port::new(self.port_addr).write(value as u32);
                }
                DataSize::QWord => {
                    unreachable!();
                }
            };
        }
    }

    pub(crate) struct MemoryAccess {
        data_size: DataSize,
        alloc: MmioAlloc,
        ptr: VirtAddr,
    }

    impl DataAccess for MemoryAccess {
        fn new(addr: usize, size: DataSize) -> Self {

            let alloc = MmioAlloc::new_from_usize(addr);
            let ptr = alloc.allocate(
                Layout::from_size_align(
                    size as u8 as usize, 1).unwrap()) // should not panic
                .expect("allocation failed");
            let ptr = VirtAddr::from_ptr(ptr.cast::<u8>().as_ptr());

            Self {
                data_size: size,
                alloc,
                ptr,
            }
        }

        unsafe fn read(&self) -> AcpiData {
            let data = match self.data_size {
                DataSize::Byte => {
                    let data: *const u8 = self.ptr.as_ptr();
                    *data as u64
                }
                DataSize::Word => {
                    let data: *const u16 = self.ptr.as_ptr();
                    *data as u64
                }
                DataSize::DWord => {
                    let data: *const u32 = self.ptr.as_ptr();
                    *data as u64
                }
                DataSize::QWord => {
                    let data: *const u64 = self.ptr.as_ptr();
                    *data as u64
                }
            };

            AcpiData {
                size: self.data_size,
                data,
            }
        }

        unsafe fn write(&mut self, value: u64) {
            match self.data_size {
                DataSize::Byte => {
                    *self.ptr.as_mut_ptr() = value as u8
                }
                DataSize::Word => {
                    *self.ptr.as_mut_ptr() = value as u16
                }
                DataSize::DWord => {
                    *self.ptr.as_mut_ptr() = value as u32
                }
                DataSize::QWord => {
                    *self.ptr.as_mut_ptr() = value as u64
                }
            }
        }
    }

    impl Drop for MemoryAccess {
        fn drop(&mut self) {
            let alloc = self.alloc;
            let addr = core::ptr::NonNull::new(self.ptr.as_mut_ptr()).unwrap(); // should not panic self.ptr comes from a NonNull within Self::new()
            unsafe {
                alloc.deallocate(
                    addr,
                    Layout::from_size_align_unchecked(self.data_size as u8 as usize, 1)); // should not fail
            }
        }

    }

    pub(crate) enum DataAccessType {
        PortAccess(PortAccess),
        MemoryAccess(MemoryAccess),
    }

    impl DataAccessType {
        pub fn read(&self) -> AcpiData {
            match self {
                DataAccessType::PortAccess(acc) => unsafe { acc.read() },
                DataAccessType::MemoryAccess(acc) => unsafe { acc.read() },
            }
        }

        #[allow(dead_code)]
        pub fn write(&mut self, value: u64) {
            match self {
                DataAccessType::PortAccess(acc) => unsafe { acc.write(value) }
                DataAccessType::MemoryAccess(acc) => unsafe { acc.write(value) }
            }
        }
    }

    impl From<GenericAddress> for DataAccessType {

        /// Converts GenericAddress into Type which is accessible without using `&dyn`
        ///
        /// Currently does not support all types. See source
        fn from(addr: GenericAddress) -> Self {
            let size = DataSize::from(addr.access_size);

            // safe casts wont destroy data. If addr is greater than usize::MAX then addr is inaccessible anyway
            match addr.address_space {
                AddressSpace::SystemMemory => Self::MemoryAccess(MemoryAccess::new(addr.address as usize, size)),
                AddressSpace::SystemIo => Self::PortAccess(PortAccess::new(addr.address as usize, size)),
                AddressSpace::PciConfigSpace => todo!(),
                AddressSpace::EmbeddedController => todo!(),
                AddressSpace::SMBus => todo!(),
                AddressSpace::SystemCmos => todo!(),
                AddressSpace::PciBarTarget => todo!(),
                AddressSpace::Ipmi => todo!(),
                AddressSpace::GeneralIo => todo!(),
                AddressSpace::GenericSerialBus => todo!(),
                AddressSpace::PlatformCommunicationsChannel => todo!(),
                AddressSpace::FunctionalFixedHardware => todo!(),
                AddressSpace::OemDefined(_) => todo!(),
            }




        }
    }
}