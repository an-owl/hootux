use core::ops::Neg;

/// This is basically just a fat pointer with a negative len
pub struct StackPointer {
    // Points to the new stack pointer
    // This may be the top of the stack, dereferencing `ptr` is unsafe because it may not be accessible
    ptr: *mut (),
    len: usize
}

impl StackPointer {

    /// Creates self from a pointer and len, where `ptr` points to the bottom of the memory region
    ///
    /// # Panics
    ///
    /// This fn will panic if len is greater than half of `usize::MAX`
    ///
    /// # Safety
    ///
    /// See [core::slice::from_raw_parts_mut] for safety information
    pub unsafe fn new_from_bottom(ptr: *mut (), len: usize) -> Self {
        Self {
            ptr: unsafe { ptr.byte_offset(len.try_into().expect("Failed to locate stack pointer, len too large")) },
            len,
        }
    }

    pub fn get_slice<'a>(&'a self) -> &'static mut [u8] {
        unsafe {
            let bottom = self.ptr.offset((self.len as isize).neg());
            core::slice::from_raw_parts_mut(bottom.cast(), self.len)
        }
    }

    pub fn get_ptr(&self) -> *mut () {
        self.ptr
    }
}