use alloc::boxed::Box;
use core::any::Any;

pub mod block;

static SYSFS_ROOT: SysFsRoot = SysFsRoot::new();

/// The sysfs allows the kernel and drivers to export system resources to make them available to
/// other components of the system.
/// The sysfs should exhibit interior mutability and prevent locking as much as possible.
struct SysFsRoot {
    block_devices: block::BlockDeviceList,
}

impl SysFsRoot {
    const fn new() -> Self {
        Self {
            block_devices: block::BlockDeviceList::new(),
        }
    }

    /// Returns a reference to the [block::BlockDeviceList]
    fn get_blk_dev(&self) -> &block::BlockDeviceList {
        &self.block_devices
    }
}

/// Returns a reference to the sysfs
fn get_sysfs() -> &'static SysFsRoot {
    &SYSFS_ROOT
}
