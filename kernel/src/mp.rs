mod init;

pub type CpuCount = core::num::NonZeroU32;
pub type CpuIndex = u32;

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
