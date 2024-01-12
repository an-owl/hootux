fn long_mode_init() -> ! {
    todo!()
}

#[repr(C)]
struct TransferSemaphore {
    data: u64,
    request: u32,
}

extern {
    fn _trampoline(); // this will never be directly called.
    static _bsp_xfer_sem: TransferSemaphore;
}