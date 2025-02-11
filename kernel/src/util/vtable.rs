

/// Returns a pointer to the vtable of `t`
///
/// # Safety
///
/// The caller must ensure that `t` is dyn Trait.
unsafe fn get_vtable<T,U>(t: &T) -> *const U {
    let u: (*const (), *const U ) = core::mem::transmute_copy(t);
    u.1
}

/// Transmutes the type of `this` into `donor` using the vtable of `donor`
///
/// # Safety
///
/// The concrete type of `this` must be the same as `donor`, and that both are dyn.
pub unsafe fn with_metadata<T,U>(this: alloc::boxed::Box<T>, donor: *const U) -> alloc::boxed::Box<U> {

    let t = (alloc::boxed::Box::leak(this) as *mut T).with_metadata_of(donor);
    alloc::boxed::Box::from_raw(t)
}