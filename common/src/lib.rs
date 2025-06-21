#![no_std]
#![feature(allocator_api)]

extern crate alloc;

use core::num::NonZeroU8;

pub mod mem {
    use core::alloc::Allocator;

    /// This is a marker trait to indicate that all methods of [Allocator]
    /// will return physically contiguous memory where the physical and virtual addresses are both
    /// aligned to [core::alloc::Layout::align].
    /// 
    /// The allocated regions may not be relocated in physical memory unless explicitly stated 
    /// (e.g via methods like [Allocator::shrink])
    pub unsafe trait PhysicalMemoryAllocator: Allocator + Translator {}
    
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
