pub mod block;

static SYSFS_ROOT: SysFsRoot = SysFsRoot::new();

/// The sysfs allows the kernel and drivers to export system resources to make them available to
/// other components of the system.
/// The sysfs should exhibit interior mutability and prevent locking as much as possible.
pub struct SysFsRoot {
    block_devices: block::BlockDeviceList,
    discovery_driver: super::driver_if::DiscoveryDriver,
}

impl SysFsRoot {
    const fn new() -> Self {
        Self {
            block_devices: block::BlockDeviceList::new(),
            discovery_driver: super::driver_if::DiscoveryDriver::new(),
        }
    }

    /// Returns a reference to the [block::BlockDeviceList]
    pub fn get_blk_dev(&self) -> &block::BlockDeviceList {
        &self.block_devices
    }

    pub fn get_discovery(&self) -> &super::driver_if::DiscoveryDriver {
        &self.discovery_driver
    }
}

/// Returns a reference to the sysfs
pub fn get_sysfs() -> &'static SysFsRoot {
    &SYSFS_ROOT
}
