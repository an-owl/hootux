use alloc::{boxed::Box, string::String, sync::Arc};
use core::fmt::{Display, Formatter};

/// This alias is for counting the number of devices available. This is an alias to help if changing
/// the size is eve required.
type BlockDevCounter = u32;

/// This Alias exists for a similar reason to [BlockDevCounter]. Its purpose is for handling block
/// device geometry.
/// At some point in the future it may be necessary to increase the size of this
type BlockDevGeomIntegral = u64;

/// This alias is for handling "async" functions which are dynamically dispatched.
/// Currently traits that contain `async` fn's cannot be made into a trait object
/// however that is required by this module.
type BoxFut<T> = Box<dyn core::future::Future<Output = T>>;

pub struct BlockDeviceList {
    list: spin::RwLock<alloc::collections::BTreeMap<BlockDeviceId, Arc<dyn SysFsBlockDevice>>>,
}

impl BlockDeviceList {
    pub(super) const fn new() -> Self {
        Self {
            list: spin::RwLock::new(alloc::collections::BTreeMap::new()),
        }
    }

    /// Registers a block device into self.
    ///
    /// # Panics
    ///
    /// This fn will panic if `device` already exists within the SysFs. It is an error for a device
    /// to be owned twice and the internal mechanism BlockDeviceList uses checks this anyway.
    pub fn register_dev<D: SysFsBlockDevice + 'static>(&self, device: D) {
        let dev = Arc::new(device);
        let id = dev.get_id();

        let ret = self.list.write().insert(id, dev);

        if ret.is_some() {
            panic!("Very very big oof. BlockDevice already exists id {}", id)
        }
    }

    /// Removes a device an all its artifacts from the SysFs
    pub fn remove_dev(&self, id: BlockDeviceId) -> Option<Arc<dyn SysFsBlockDevice>> {
        self.list.write().remove(&id)
    }

    /// Returns a reference to the the requested block device. If it exists
    pub fn fetch(&self, id: BlockDeviceId) -> Option<Arc<dyn SysFsBlockDevice>> {
        // unwraps result derefs the box clones it and re-boxes it
        Some(self.list.read().get(&id)?.clone())
    }
}

/// A Unique identifier for a block device.
///
/// This struct is made up of components that allow a driver to quickly and easily provide an
/// identity to a block device. This struct is composed of a reference to the name of the driver
/// that manages the device and its instance number which are provided by the system upon the
/// creation of this struct. An instance of a driver refers to the task which manages the device.
/// A driver may choose to spawn as many or as few instances as it wants per device.
// hey as long as it works
/// The driver must provide a identifier which is unique to the instance, if an instance manages
/// multiple devices this may be set to `None` and it will be omitted. The identifiers have no
/// specific purpose apart from identification and therefore are not required to follow any specific
/// convention.
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct BlockDeviceId {
    name: &'static str,
    instance: usize,
    device: Option<usize>,
}

#[non_exhaustive]
#[derive(Eq, PartialEq, Debug, Copy, Clone)]
pub enum BlockDevIoErr {
    /// The buffer size is not aligned to the geometry specified by the device.
    Misaligned,
    /// Attempted to access a block outside of the devices range.
    OutOfRange,
    /// General geometry error.
    GeomError,
    /// The driver encountered an error while in operation that prevented it from completing the
    /// requested operation.
    HardwareError,
    /// The driver itself encountered a software error.
    ///
    /// This should **only** be used to indicate bugs. It is reasonable to panic if this error occurs
    InternalDriverErr,
    /// The device had been disabled or removed and may not be accessed anymore. All further
    /// operations will return this error.
    DeviceOffline,
}

impl Display for BlockDeviceId {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        // ahci,0,15 will be "ahci-0-15"
        // drv,1,None will be "drv-1"
        write!(
            f,
            "{}-{}{}",
            self.name,
            self.instance,
            self.device
                .map_or(String::new(), |f| alloc::format!("-{}", f))
        )
    }
}

/// This trait is for interacting with block devices.
///
/// Implementations of this trait should use this trait as a supertrait instead of exposing it directly.
///
/// Block device operations are done in blocks (hence the name) the size of these blocks can be
/// retrieved using [Self::geom]. All operations must be aligned to the block size however operations
/// should always use the optimal block size where possible.
pub trait BlockDev: Sync + Send {
    /// Reads `T` from the device. Implementations may return Err(_) if the size of `T` is not
    /// aligned to [Self::get_bs]
    fn read(&self, seek: BlockDevGeom, size: usize) -> BoxFut<Result<Box<[u8]>, BlockDevIoErr>>;

    /// Writes the given buffer onto the device. The buffer size should be aligned to the devices
    /// block size, this fn may return Err(_) if it is not. A reference to the buffer is returned on completion.
    fn write(
        &self,
        seek: BlockDevGeom,
        buff: &'static [u8],
    ) -> BoxFut<Result<&'static [u8], BlockDevIoErr>>;

    /// Returns a struct containing the geometry of the device.
    /// The device geometry must include the block size of the device and the number of blocks in
    /// the device.
    fn geom(&self) -> BlockDevGeom;

    fn as_any(&self) -> &dyn core::any::Any;
}

pub struct BlockDevGeom {
    /// The number of blocks this device contains.
    pub blocks: BlockDevGeomIntegral,

    /// The size in bytes per block, all operations are aligned to this size
    pub bs: BlockDevGeomIntegral,

    /// The optimal block size in bytes. Some devices emulate different sized blocks for compatibility,
    /// this can slow down operations. This size should be used instead of [self.bs]  
    pub obs: BlockDevGeomIntegral,

    /// The largest transfer this device is capable of in a single transfer
    /// Software may extend this value over multiple operations, if it doesnt is must return [BlockDevIoErr::GeomError] if the size is to large
    pub max_blocks_per_transfer: BlockDevGeomIntegral,
}

/// Trait for block devices which are stored within the in the SysFs [BlockDeviceList]. Not
/// all block devices should appear within this list. Partitions can be exported as block devices
/// but are actually an interface to a hardware device (probably a `SysFsBlockDevice`).
///
/// This trait should only be implemented by structs that directly represent hardware devices
///
/// Implementations should use some form of reference counting
pub trait SysFsBlockDevice: BlockDev {
    /// Returns the unique id of the device
    fn get_id(&self) -> BlockDeviceId;
}
