#![no_std]
#![feature(allocator_api)]

extern crate alloc;

use core::num::NonZeroU8;

pub mod mem {
    use core::alloc::Allocator;

    /// This is a trait that indicate that all methods of [Allocator] and are 
    /// will return physically contiguous memory where the physical and virtual addresses are both
    /// aligned to [core::alloc::Layout::align].
    /// 
    /// The allocated regions may not be relocated in physical memory unless explicitly stated 
    /// (e.g via methods like [Allocator::shrink])
    /// 
    /// A physical memory allocator must allow the caller to specify what [DmaRegion] it requires.
    pub unsafe trait PhysicalMemoryAllocator: Allocator + Translator + Clone + Copy {
        fn set_region(self, region: DmaRegion) -> Self;
    }
    
    /// Helper trait for smart pointer types.
    pub trait Translate {
        
        /// This method calls [Translator::translate] on the value pointed to by this object.
        fn translate(&self) -> Option<u64>;
    }
    
    pub trait Translator {
        
        /// This method returns the physical address of `addr`. 
        fn translate<T: ?Sized>(&self, addr: *const T) -> Option<u64>;
    }
    
    impl<T: Allocator + Translator, U> Translate for alloc::boxed::Box<U,T> {
        fn translate(&self) -> Option<u64> {
            alloc::boxed::Box::allocator(self).translate(&**self)
        }
    }
    impl<T: Allocator + Translator, U> Translate for alloc::sync::Arc<U,T> {
        fn translate(&self) -> Option<u64> {
            alloc::sync::Arc::allocator(self).translate(&**self)
        }
    }
    
    /// `Mapper` provides an interface to allow fetching a pointer to a specified physical address.
    /// 
    /// This trait defines an implementation of [Allocator] which can allocate certain physical addresses.
    /// `Allocator` methods *must* return aliased memory when requested. The caller must ensure that aliasing rules are obeyed.
    /// 
    /// The implementation must ensure that the entire region described by `layout` is mapped in its entirety.
    /// If `addr` is not aligned to `layout.align()` then the implementation must use the 
    /// `pointer_offset + layout.size` as the total size of the mapped region. 
    /// The returned pointer **must** point to `addr`, the caller is responsible for alignment.
    ///
    /// ```
    ///  # #![feature(allocator_api)]
    ///  # use std::alloc::Allocator; 
    ///  # fn map(mapper: &mut impl common::mem::Mapper, translator: &impl common::mem::Translator) {
    ///         let layout = core::alloc::Layout::from_size_align(8,8).unwrap();
    ///         let tgt_addr = 7;
    ///         let mapper = mapper.set_addr(tgt_addr);
    ///         let ptr = Allocator::allocate(&mapper,layout).unwrap();
    ///
    ///         assert_eq!(translator.translate(&ptr.as_ptr()).unwrap(),tgt_addr); // Physical address is guaranteed to be the requested address
    ///         // pointer is aligned to `8`, the remaining size 8 bytes must still be mapped.
    ///         // The region here points to the physical address range 7..16
    ///         assert_eq!(ptr.as_ptr().len(),9); 
    ///  # } 
    /// ```
    pub trait Mapper: Allocator + Copy + Clone {
        
        /// Returns `Self` targeted at the physical address `addr`
        fn set_addr(self, addr: u64) -> Self;
    }

    /// The DMA region describes memory ranges.
    /// 
    /// A 16bit DMA region is not defined here as it is irrelevant to USB.
    /// 
    /// Variants may be decremented into smaller regions where necessary, but may not incremented 
    /// into larger regions.
    #[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Default)]
    pub enum DmaRegion {
        /// Describes the range 0..4GiB
        #[cfg_attr(target_pointer_width = "32",default)]
        Dma32,
        /// Describes the region 4GiB..16EiB
        #[cfg_attr(target_pointer_width = "64",default)]
        Dma64,
    }
}

pub enum TargetSpeed {
    LowSpeed,
    FullSpeed,
    HighSpeed,
    SuperSpeed,
}

#[derive(Debug,Eq, PartialEq, Ord, PartialOrd, Copy, Clone)]
pub enum Address {
    Broadcast,
    AddrNum(AddrNum)
}

impl Address {
    pub const fn from_int(num: u8) -> Option<Self> {
        match num {
            0 => Some(Address::Broadcast),
            // SAFETY: This takes a
            n @ 1..127 => Some(Address::AddrNum(AddrNum( unsafe { NonZeroU8::new_unchecked(n) }))),
            _ => None,
        }
    }
}

impl From<Address> for u8 {
    fn from(addr: Address) -> Self {
        match addr {
            Address::Broadcast => 0,
            Address::AddrNum(n) => n.0.get()
        }
    }
}

#[derive(Debug, Ord, PartialOrd, Eq, PartialEq, Copy, Clone)]
pub struct AddrNum(NonZeroU8);

#[derive(Debug, Ord, PartialOrd, Eq, PartialEq, Copy, Clone)]
pub struct Target {
    address: Address,
    endpoint: Endpoint
}

impl Target {
    pub const fn new(address: Address, endpoint: Endpoint) -> Self {
        Target { address, endpoint }
    }

    pub const fn address(&self) -> Address {
        self.address
    }

    pub const fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

}

#[derive(Debug, Ord, PartialOrd, Eq, PartialEq, Copy, Clone)]
pub struct Endpoint(u8);

impl Endpoint {
    pub const fn new(num: u8) -> Option<Self> {
        match num {
            n @ 0..16 => Some(Self(n)),
            _ => None
        }
    }
}

impl From<Endpoint> for u8 {
    fn from(ep: Endpoint) -> Self {
        ep.0
    }
}
 