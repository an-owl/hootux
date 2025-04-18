// todo make this configurable
pub const PHYS_OFFSET_ADDR: usize = {
    #[cfg(target_pointer_width = "64")]
    {
        0xFFFF808000000000 // entry 2 of l4 table
    }
    #[cfg(not(target_pointer_width = "64"))]
    0
};

// todo make configurable
pub const STACK_SIZE: usize = 0x100000;

/// Unused for EFI
/// Indicates the top of the stack. Note that on x86 targets the indicated address should not be mapped.
// l4 entry #510 last 4K page is stack guard.
pub const STACK_ADDR: usize = 0xFFFFFFFFF000;
