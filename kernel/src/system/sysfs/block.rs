//! A block device is a special file which allows access to hardware using a file-like interface.
//! I/O performed on block devices must conform to particular sizes, which are specified by the
//! block device itself.
//!
//! The kernel stores a list of block devices which are registered by device drivers.
//! Implementations of [BlockDev] can be cloned to create multiple references to the same device.
//! At any point the hardware device may become unavailable if this occurs the [BlockDev] should be
//! dropped as the device will never return.

use alloc::{boxed::Box, string::String};
use log::warn;

/// This Alias exists for a similar reason to [BlockDevCounter]. Its purpose is for handling block
/// device geometry.
/// At some point in the future it may be necessary to increase the size of this
pub type BlockDevGeomIntegral = u64;

/// This alias is for handling "async" functions which are dynamically dispatched.
/// Currently traits that contain `async` fn's cannot be made into a trait object
/// however that is required by this module.
pub type IoFut<'a, T> = futures_util::future::BoxFuture<'a, Result<T, BlockDevIoErr>>;

pub struct BlockDeviceList {
    list: spin::RwLock<alloc::collections::BTreeMap<BlockDeviceId, Box<dyn SysFsBlockDevice>>>,
}

impl BlockDeviceList {
    pub(super) const fn new() -> Self {
        Self {
            list: spin::RwLock::new(alloc::collections::BTreeMap::new()),
        }
    }

    /// Registers a block device into self.
    /// This fn will return `device` if the block device is already registered.
    pub fn register_dev(
        &self,
        device: Box<dyn SysFsBlockDevice>,
    ) -> Option<Box<dyn SysFsBlockDevice>> {
        let id = device.get_id();
        log::debug!("registered {id}");
        let mut ret = self.list.write().insert(id, device);
        if ret.is_some() {
            warn!("Driver attempted to register block device {id} twice");
            ret = self.list.write().insert(id, ret.unwrap())
        }

        ret
    }

    /// Removes a device an all its artifacts from the SysFs
    pub fn remove_dev(&self, id: BlockDeviceId) -> Option<Box<dyn SysFsBlockDevice>> {
        self.list.write().remove(&id)
    }

    /// Returns a copy of the requested block device, if it exists.
    pub fn fetch(&self, id: BlockDeviceId) -> Option<Box<dyn SysFsBlockDevice>> {
        Some(self.list.read().get(&id)?.clone())
    }

    /// Returns a list of all system managed block devices.
    pub fn list(&self) -> alloc::vec::Vec<BlockDeviceId> {
        self.list.read().keys().map(|k| k.clone()).collect()
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
// todo consider changing this to a trait object
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
pub struct BlockDeviceId {
    name: &'static str,
    instance: usize,
    device: Option<usize>,
}

impl BlockDeviceId {
    pub fn new(name: &'static str, instance: usize, device: Option<usize>) -> Self {
        Self {
            name,
            instance,
            device,
        }
    }
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

impl core::fmt::Display for BlockDeviceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // ahci,0,15 will be "ahci-0-15"
        // drv,1,None will be "drv-1"
        write!(
            f,
            "{}-{}:{}",
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
///
/// Methods that take profiles as arguments will likely be added in the future to help optimize I/O.
///
/// Implementor note: If a future returns `Err(BlockDevError::DeviceOffline)` the block device will
/// not automatically be removed from any list and the implementation should do this itself.
pub trait BlockDev: Sync + Send {
    /// Reads `size` blocks from from the device starting at the `seek` block. Implementations may
    /// return `Err(BlockDevIoErr::GeomError)` if `size` is not aligned to `self.geom().block_size`
    fn read(&self, seek: BlockDevGeomIntegral, size: usize) -> IoFut<Box<[u8]>>;

    /// Writes the given buffer onto the device. The buffer size should be aligned to the devices
    /// block size, this fn may return Err(_) if it is not. A reference to the buffer is returned on completion.
    ///
    /// implementor note: for DMA that cannot be stopped the future should be disowned and completed.
    ///
    /// # Implementation Safety
    ///
    /// If the future is dropped it should attempt to either stop the DMA or disown itself and
    /// complete outside of the current task.
    /// If `core::mem::forget(self.write(_).poll(_))` is called the buffer will be leaked onto memory
    /// and the DMA will be completed.
    fn write(&self, seek: BlockDevGeomIntegral, buff: IoBuffer) -> IoFut<IoBuffer>;

    /// Returns a struct containing the geometry of the device.
    /// The device geometry must include the block size of the device and the number of blocks in
    /// the device.
    ///
    /// This fn is `async` because retrieving the device geometry may require querying hardware.
    /// Implementations should not return [core::task::Poll::Waiting] if possible to enable calling
    /// fn's to block.
    ///
    /// If this fn returns `Err(_)` the hardware device can be considered failed.
    fn geom(&self) -> IoFut<BlockDevGeom>;

    fn as_any(&self) -> &dyn core::any::Any;

    fn b_clone(self: &Self) -> Box<dyn BlockDev>;
}

/// Wrapper for data used for DMA with block devices.
///
/// Certain restrictions must be placed on IO buffers to ensure memory safety.
/// Usually this can be done by passing a static reference to the data.
/// However limitations of block devices and implementation specific details require extra restrictions.
/// This struct ensures that data will not be modified while DMA is working, and that data can be
/// accessed once it has completed.
// todo this is a stopgap solution and should be replaced with copy-on-write
#[derive(Clone)]
pub struct IoBuffer {
    inner: core::ptr::NonNull<IoBufferInner>,
}

struct IoBufferInner {
    count: core::sync::atomic::AtomicUsize,
    data: Box<[u8]>,
}

// SAFETY: Inner data is not accessible mutably.
// Unwrapping asserts that the caller is the sole owner of the data
unsafe impl Send for IoBuffer {}
unsafe impl Sync for IoBuffer {}

impl IoBuffer {
    pub fn new(data: Box<[u8]>) -> Self {
        let t = Box::leak(Box::new(IoBufferInner {
            count: 0.into(),
            data,
        }))
        .into();

        Self { inner: t }
    }

    pub fn try_extract(mut self) -> Result<Box<[u8]>, Self> {
        // SAFETY: This is safe because it points to valid data and only accesses an atomic
        if unsafe { self.inner.as_ref().count.load(atomic::Ordering::Relaxed) } == 1 {
            // SAFETY: This is save because the data is not referenced anywhere else
            unsafe {
                let IoBufferInner { data, .. } = core::ptr::read(self.inner.as_ptr());

                // SAFETY: This is safe because this needs to be dropped
                alloc::alloc::dealloc(
                    self.inner.cast().as_ptr(),
                    core::alloc::Layout::new::<IoBufferInner>(),
                );

                Ok(data)
            }
        } else {
            Err(self)
        }
    }

    pub fn len(&self) -> usize {
        // SAFETY: This is safe because there are not mutable references to *self.inner
        unsafe { self.inner.as_ref().data.len() }
    }

    /// Returns the address of the data referenced by `self`
    pub fn addr(&self) -> usize {
        // SAFETY: No mutable references exist to *self.inner
        unsafe { self.inner.as_ref().data.as_ptr() as usize }
    }

    /// Returns a slice containing the buffer
    pub fn buff(&self) -> &[u8] {
        // SAFETY: No mutable references exist to *self.inner
        unsafe { self.inner.as_ref().data.as_ref() }
    }
}

impl Drop for IoBuffer {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: inner is always a valid pointer, this is only accessing a filed with interior mutability.
            if self
                .inner
                .as_ref()
                .count
                .fetch_sub(1, atomic::Ordering::Release)
                == 0
            {
                // SAFETY: *self.inner is constructed from a box anyway
                drop(Box::from_raw(self.inner.as_ptr()))
            }
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct BlockDevGeom {
    /// The number of blocks this device contains.
    pub blocks: BlockDevGeomIntegral,

    /// The size in bytes per block, all operations are aligned to this size
    pub block_size: BlockDevGeomIntegral,

    /// The optimal block size in bytes. Some devices emulate different sized blocks for compatibility,
    /// this can slow down operations. This size should be used instead of `self.bs`  
    pub optimal_block_size: BlockDevGeomIntegral,

    /// Optimal blocks might not be aligned to `self.optimal_block_size`. `self.blocks + self.optimal_alignment`
    /// will be the start of the first optimal block.   
    pub optimal_alignment: BlockDevGeomIntegral,

    /// The largest transfer this device is capable of in a single transfer
    /// Software may extend this value over multiple operations, if it doesnt is must return
    /// [BlockDevIoErr::GeomError] if the size is to large
    pub max_blocks_per_transfer: BlockDevGeomIntegral,

    /// Data is required to be aligned to this value in order to be written to the block device.
    ///
    /// Drivers should not automatically re-align data unless it can be done without a `memcpy`
    pub req_data_alignment: usize,
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

    fn s_clone(self: &Self) -> Box<dyn SysFsBlockDevice>;
}

impl Clone for Box<dyn BlockDev> {
    fn clone(&self) -> Self {
        self.b_clone()
    }
}

impl Clone for Box<dyn SysFsBlockDevice> {
    fn clone(&self) -> Self {
        self.s_clone()
    }
}
