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
