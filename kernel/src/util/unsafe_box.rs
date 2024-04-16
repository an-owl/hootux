use core::fmt::{Debug, Formatter};

/// A box to contain a memory allocation without explicitly referencing memory
/// The purpose is to allow multiple references to the contained value without explicitly aliasing
/// the contained memory.
///  
/// ## Dropping
///
/// This will not drop contained values, however the contained memory will be freed.
/// If a struct contains references to an UnsafeBox the box should be placed in the struct lower
/// than anything pointing to the contained data, this is because struct fields are dropped in the
/// order they are declared.
/// ```
/// struct Foo {
///     dropped_first: &'static u8, // references the data owned by `dropped_second`
///     dropped_second: UnsafeBox<u8>
/// }
/// ```
pub(crate) struct UnsafeBox<T, A: alloc::alloc::Allocator = alloc::alloc::Global> {
    _pin: core::marker::PhantomData<T>,
    alloc: A,
    ptr: *mut T,
}

impl<T, A: alloc::alloc::Allocator> UnsafeBox<T, A> {
    pub fn new(alloc: A) -> Self {
        let ptr = alloc
            .allocate(alloc::alloc::Layout::new::<T>())
            .expect("System ran out of memory")
            .as_ptr()
            .cast();
        Self {
            _pin: Default::default(),
            alloc,
            ptr,
        }
    }

    /// Returns a pointer to the contained value.
    ///
    /// # Safety
    ///
    /// Normal raw pointer safety rules apply.
    /// The caller must ensure that `self` is dropped after all references are dropped
    pub unsafe fn ptr(&self) -> *mut T {
        self.ptr
    }
}

impl<T, A: alloc::alloc::Allocator> Drop for UnsafeBox<T, A> {
    fn drop(&mut self) {
        unsafe {
            self.alloc.deallocate(
                core::ptr::NonNull::new(self.ptr.cast()).unwrap(),
                alloc::alloc::Layout::new::<T>(),
            )
        }
    }
}

impl<T, A: alloc::alloc::Allocator> Debug for UnsafeBox<T, A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut d = f.debug_struct(core::any::type_name::<Self>());
        d.field("ptr", &alloc::format!("{:#x}", self.ptr as usize));

        d.finish()
    }
}

// SAFETY: This is safe as long as safety rules are obeyed
unsafe impl<T, A: alloc::alloc::Allocator> Send for UnsafeBox<T, A> where A: Send {}
unsafe impl<T, A: alloc::alloc::Allocator> Sync for UnsafeBox<T, A> where A: Sync {}
