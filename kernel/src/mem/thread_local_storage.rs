//! This module is for initializing thread local storage.
//! At this point it only supports the initial exec model. In this model the `fs` register points to
//! a pointer to the thread local structure. The thread local data is located below the address
//! pointed to by the data at `fs`.

use alloc::boxed::Box;
use x86_msr::Msr;

const TLS_ALIGN: usize = 4096;

/// Creates a region of `mem_size` which contains thread local data. This function will allocate
/// `mem_size` bytes onto the heap. The returned pointer points to the uninitialized Thread Control
/// Block.
///
/// # Saftey
///
/// This function is unsafe because the caller must ensure that all args correctly describe the
/// thread local template.
unsafe fn create_tls(t_data: *const u8, file_size: usize, mem_size: usize) -> *const u8 {
    unsafe {
        let layout = core::alloc::Layout::from_size_align(mem_size, TLS_ALIGN).unwrap();
        let region = core::slice::from_raw_parts_mut(alloc::alloc::alloc(layout), mem_size);

        let template = core::slice::from_raw_parts(t_data, file_size);
        region[..file_size].clone_from_slice(template);
        region[file_size..mem_size].fill_with(|| 0);

        let ptr = region.as_ptr() as usize;
        let tcb_ptr = (ptr + mem_size) as *const u8;
        let offset = tcb_ptr.align_offset(TLS_ALIGN);
        let tcb_aligned = (tcb_ptr as usize + offset) as *const u8;

        tcb_aligned
    }
}

/// Creates a new TLS and returns a pointer to it. The pointer should be stored in the `IA32_GSBASE` MSR of the CPU it will be used on.
///
/// # Safety
///
/// This fn us unsafe because the caller must ensure that the arguments correctly describe the thread local template.
/// The arguments given at runtime should never change.
pub unsafe fn new_tls(t_data: *const u8, file_size: usize, mem_size: usize) -> *const *const u8 {
    unsafe {
        let tp = create_tls(t_data, file_size, mem_size);
        Box::leak(Box::new(tp)) as *const *const u8
    }
}

/// Creates and initializes a thread local template for this CPU.
/// This will set the systems RunLevel to Init
///
/// This function will leak `mem_size` bytes onto the heap
///
/// # Safety
///
/// This function is unsafe because the caller must ensure that the given args properly describe
/// the thread local template.
pub unsafe fn init_tls(t_data: *const u8, file_size: usize, mem_size: usize) {
    unsafe {
        let tp = new_tls(t_data, file_size, mem_size);

        x86_msr::architecture::FsBase::write(tp.into());
        crate::runlevel::update_runlevel(crate::runlevel::Runlevel::Init)
    }
}
