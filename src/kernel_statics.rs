use alloc::boxed::Box;
use core::cell::RefCell;
use core::mem::MaybeUninit;
use x86_64::{PhysAddr, VirtAddr};
use spin::Mutex;
use x86_64::structures::paging::{Mapper, Page, Size4KiB};
use crate::mem::{
    BootInfoFrameAllocator,
    page_table_tree::PageTableTree
};


pub(crate) static mut LOCAL: RefCell<MaybeUninit<KernelLocals>> = RefCell::new(MaybeUninit::uninit());

/// a wrapper to help remove boilerplate code while fetching `LOCAL`
#[inline]
pub(crate) fn fetch_local() -> &'static mut KernelLocals{
    unsafe { LOCAL.get_mut().assume_init_mut() }
}

// everything in here should be mutex.
// thread_local is an exception, it local should never
// point to the same Physical frame as another cpu.
/// This struct is used to store system global variables.
/// It should NEVER be mutable, all variables that require mutability are `Mutex<T>`.
/// This is because all values in this struct may be accessed  at any time by any cpu
/// they should not be used during interrupts
/// Only one of these should exist at a time
///
/// It is aligned to 4096
#[repr(align(4096),C)]
pub(crate) struct KernelGlobals {
    pub frame_alloc: Mutex<BootInfoFrameAllocator>,
    pub self_phys_addr: PhysAddr, // required to re_map frame; NEVER EVER change after creation.
    pub(crate) logger: crate::logger::Logger // internally uses mutex
}

impl KernelGlobals {
    /// Creates an initialized instance of self
    ///
    /// This is required because `self_phys_addr` cannot be set until self is created.
    pub(crate) const fn new_without_addr(frame_alloc: BootInfoFrameAllocator) -> Self {
        Self {
            self_phys_addr: PhysAddr::zero(),
            frame_alloc: Mutex::new(frame_alloc),
            logger: crate::logger::Logger::new()
        }
    }

    /// sets `self.self_phys_addr` to `addr`.
    ///
    /// This function is unsafe because the caller muse ensure that `addr` is correct,
    /// otherwise this may cause invalid memory to be referenced.
    unsafe fn set_addr(&mut self, addr: PhysAddr) {
        self.self_phys_addr = addr
    }
}

#[repr(align(4096),C)]
pub(crate) struct KernelLocals {
    pub page_table_tree: PageTableTree,
    kernel_globals: &'static KernelGlobals,
}

impl KernelLocals {
    /// Initializes self
    pub (crate) fn init(globals: KernelGlobals , tree: PageTableTree) -> Self {
        let mut kernel_globals = Box::new(globals); // probably give this its on allocator, write once kind of thing

        let phys_addr = tree.translate_page(Page::<Size4KiB>::containing_address(VirtAddr::from_ptr(&*kernel_globals))).unwrap().start_address();
        unsafe { kernel_globals.set_addr(phys_addr) };

        Self{
            page_table_tree: tree,
            kernel_globals: Box::leak(kernel_globals),
        }
    }

    /// Returns kernel_globals as a reference
    pub fn globals(&self) -> &'static KernelGlobals{
        self.kernel_globals
    }
}