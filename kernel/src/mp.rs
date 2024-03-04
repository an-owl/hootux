
mod init;

pub type CpuCount = core::num::NonZeroU32;
pub type CpuIndex = u32;

/// Starts up all available Application Processors
///
/// # Safety
///
/// This fn is unsafe because the caller must ensure that `tls_data` `tls_file_size` and `tls_data_size`
/// correctly describe the thread local template.
///
/// This fn may not be called more than once.
pub unsafe fn start_mp(tls_data: *const u8, tls_file_size: usize, tls_data_size: usize) {
    #[cfg(feature = "multiprocessing")]
    init::start_mp(tls_data,tls_file_size,tls_data_size)
}

pub fn num_cpus() -> CpuCount {
    static NUM: atomic::Atomic<Option<CpuCount>> = atomic::Atomic::new(None);
    if let Some(n) = NUM.load(atomic::Ordering::Relaxed) {
        n
    } else {
        // This is racy... kind of. It may be called multiple times but all the resources
        // it accesses are static, global and immutable.
        // This will always have the same result
        let madt = crate::system::sysfs::get_sysfs()
            .firmware()
            .get_acpi()
            .find_table::<acpi::madt::Madt>()
            .expect("No MADT found");
        let (_, entries) = madt.parse_interrupt_model_in(alloc::alloc::Global).unwrap();
        let r = ((entries.unwrap().application_processors.len() + 1) as u32)
            .try_into()
            .unwrap();
        NUM.store(Some(r), atomic::Ordering::Relaxed);
        r
    }
}

/// Returns the ID of the current CPU.
pub fn who_am_i() -> CpuIndex {
    crate::who_am_i()
}