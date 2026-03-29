//! This module contains bindings to llvm intrinsic functions which do not already have rust
//! bindings in [core::intrinsics].
//!
//! Everything within this module is unstable.

unsafe extern "C" {

    /// Fetches the return address of the current fn.
    /// This will ignore inlining.
    ///
    /// `frame` contains the requested frame ID (0 for the current frame)
    /// for info see [llvm documentation](https://llvm.org/docs/LangRef.html#llvm-returnaddress-intrinsic)
    ///
    /// This should not be called directly.
    #[doc(hidden)]
    #[link_name = "llvm.returnaddress"]
    pub fn private_return_address(frame: i32) -> *const ();

    #[doc(hidden)]
    #[link_name = "llvm.addressofreturnaddress"]
    pub fn _private_address_of_return_address(frame: i32) -> *mut *const ();
}

/// Fetches the return address of the current fn.
/// This will ignore inlining.
/// For info see [llvm documentation](https://llvm.org/docs/LangRef.html#llvm-returnaddress-intrinsic)
///
/// This macro returns an Option<*const ()>
///
/// # Safety
///
/// This fn is not unsafe in its current state (rustc 1.77.0), due to its instability it is treated as unsafe
#[macro_export]
macro_rules! return_address {
    () => {{
        let __hootux_return_address_macro_binding = $crate::llvm::private_return_address(0);
        if __hootux_return_address_macro_binding.is_null() {
            None
        } else {
            Some(__hootux_return_address_macro_binding)
        }
    }};
}

/// Returns the address of the return address, allowing it to be modified.
/// This will ignore inlining.
/// For info see [llvm documentation](https://llvm.org/docs/LangRef.html#llvm-addressofreturnaddress-intrinsic)
///
/// # Safety
///
/// This fn is not unsafe but it is unstable. Implementation is designed for LLVM 21.1.8
macro_rules! address_of_return_address {
    () => {{
        let rc = $crate::llvm::_private_address_of_return_address(0);
        if rc.is_null() { None } else { Some(rc) }
    }};
}
pub(crate) use address_of_return_address;
