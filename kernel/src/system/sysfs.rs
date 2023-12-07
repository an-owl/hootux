pub mod block;
pub(crate) mod systemctl;
static SYSFS_ROOT: SysFsRoot = SysFsRoot::new();

/// The sysfs allows the kernel and drivers to export system resources to make them available to
/// other components of the system.
/// The sysfs should exhibit interior mutability and prevent locking as much as possible.
pub struct SysFsRoot {
    block_devices: block::BlockDeviceList,
    discovery_driver: super::driver_if::DiscoveryDriver,
    firmware: Firmware,
    /// This is not supposed to be exported to user mode, this contains structures that the kernel uses
    /// to access hardware devices used to configure system operation.
    pub(crate) systemctl: systemctl::SystemctlResources,
}

impl SysFsRoot {
    const fn new() -> Self {
        Self {
            block_devices: block::BlockDeviceList::new(),
            discovery_driver: super::driver_if::DiscoveryDriver::new(),
            firmware: Firmware::new(),
            systemctl: systemctl::SystemctlResources::new(),
        }
    }

    /// Returns a reference to the [block::BlockDeviceList]
    pub fn get_blk_dev(&self) -> &block::BlockDeviceList {
        &self.block_devices
    }

    pub fn get_discovery(&self) -> &super::driver_if::DiscoveryDriver {
        &self.discovery_driver
    }

    pub fn firmware(&self) -> &Firmware {
        &self.firmware
    }

    pub fn setup_ioapic(&self, madt: &acpi::madt::Madt) {
        self.systemctl.ioapic.cfg_madt(madt)
    }
}

/// Returns a reference to the sysfs
pub fn get_sysfs() -> &'static SysFsRoot {
    &SYSFS_ROOT
}

/// Contains structures provided by system firmware.
/// These are initialized before the runlevel is set to [crate::runlevel::Runlevel::Kernel],
/// any fields which are `None` after this point were not provided by the firmware.
pub struct Firmware {
    acpi: crate::kernel_structures::Worm<AcpiRoot>,
}

impl Firmware {
    const fn new() -> Self {
        Self {
            acpi: crate::kernel_structures::Worm::new(),
        }
    }

    /// Sets `tables` as the system ACPI resource.
    ///
    /// # Panics
    ///
    /// This fn will panic if called more than once
    ///
    /// # Safety
    ///
    /// This fn is racy and must only be called before MP initialization.
    pub unsafe fn cfg_acpi(&self, tables: acpi::AcpiTables<super::acpi::AcpiGrabber>) {
        log::trace!("ACPI global set");
        self.acpi.write(AcpiRoot(tables))
    }

    /// Returns the system [acpi::AcpiTables] structure.
    ///
    /// # Panics
    ///
    /// This fn will panic if [Self::cfg_acpi] has not been called.
    ///
    /// # Safety
    ///
    /// This fn is marked as safe, however it is unsafe to call it before MP initialization.
    pub fn get_acpi(&self) -> &acpi::AcpiTables<super::acpi::AcpiGrabber> {
        &self.acpi.read().0
    }
}

struct AcpiRoot(acpi::AcpiTables<super::acpi::AcpiGrabber>);

// SAFETY: fixme I'm not actually sure if this is safe. Just dont allow mutable access to it I guess.
unsafe impl Sync for AcpiRoot {}
unsafe impl Send for AcpiRoot {}
