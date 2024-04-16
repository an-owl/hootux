
mod init;
pub mod bitmap;

pub type CpuCount = u32;
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

/// Contains the number of currently running CPUs.
/// Incremented when each CPU AP is started.
static NUM_CPUS: atomic::Atomic<CpuCount> = atomic::Atomic::new(1);

/// Increments the number of CPUs by 1, This is called at the AP entry point.
fn cpu_start() {
    NUM_CPUS.fetch_add(1,atomic::Ordering::Release);
}

/// Returns the number of CPUs which are currently running, if you need the number of CPUs installed
/// on the system this must be fetched from the ACPI tables.
pub fn num_cpus() -> CpuCount {
    NUM_CPUS.load(atomic::Ordering::Relaxed)
}

/// Returns the ID of the current CPU.
pub fn who_am_i() -> CpuIndex {
    crate::who_am_i()
}