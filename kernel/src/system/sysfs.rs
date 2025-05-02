pub mod block;
pub(crate) mod systemctl;
static SYSFS_ROOT: SysFsRoot = SysFsRoot::new();

/// The sysfs allows the kernel and drivers to export system resources to make them available to
/// other components of the system.
/// The sysfs should exhibit interior mutability and prevent locking as much as possible.
pub struct SysFsRoot {
    block_devices: block::BlockDeviceList,
    discovery_driver: super::driver_if::DiscoveryDriver,
    /// This is not supposed to be exported to user mode, this contains structures that the kernel uses
    /// to access hardware devices used to configure system operation.
    pub(crate) systemctl: systemctl::SystemctlResources,
}

impl SysFsRoot {
    const fn new() -> Self {
        Self {
            block_devices: block::BlockDeviceList::new(),
            discovery_driver: super::driver_if::DiscoveryDriver::new(),
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

    pub fn setup_ioapic(&self, madt: core::pin::Pin<&acpi::madt::Madt>) {
        self.systemctl.ioapic.cfg_madt(madt)
    }
}

/// Returns a reference to the sysfs
pub fn get_sysfs() -> &'static SysFsRoot {
    &SYSFS_ROOT
}

struct AcpiRoot(acpi::AcpiTables<super::acpi::AcpiGrabber>);

// SAFETY: fixme I'm not actually sure if this is safe. Just dont allow mutable access to it I guess.
unsafe impl Sync for AcpiRoot {}
unsafe impl Send for AcpiRoot {}
