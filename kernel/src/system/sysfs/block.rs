use alloc::string::{String, ToString};
use core::fmt::Display;
use core::ops::{Deref, Index};

type BlockDevCounter = u32;
type BlockDevGeom = u64;

pub struct BlockDeviceList {
    list: spin::RwLock<
        alloc::collections::BTreeMap<BlockDevId, alloc::boxed::Box<dyn SysFsBlockDevice>>,
    >,
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
    pub fn register_dev<D: SysFsBlockDevice>(&self, device: D) {
        let dev = alloc::boxed::Box::new(device);
        let id = dev.get_id();

        let ret = self.list.write().insert(id, dev);

        if ret.is_some() {
            panic!(
                "Very very big off. BlockDevice already exists id {}",
                id.token()
            )
        }
    }

    /// Removes a device an all its artifacts from the SysFs
    pub fn remove_dev(&self, id: BlockDeviceId) -> Option<alloc::boxed::Box<dyn BlockDevClass>> {
        self.list.write().remove(&id)
    }

    /// Returns a reference to the the requested block device. If it exists
    pub fn fetch(&self, id: BlockDeviceId) -> Option<&dyn SysFsBlockDevice> {
        Some(&**self.list.read().get(&id)?)
    }
}

/// SysFsBlockDevice is intended to contain controls for interacting with a block device that is
/// exported via its driver.
///
/// An implementation should exhibit certain behaviours such as interior mutability.
/// Interior mutability is required to allow mutable assesses without requiring mutable
/// access to parent structures. Which allows multiple CPUs to access system resources at the same
/// time.
///
/// A block device may have artifacts linked to it such as partitions. A block device should be
/// aware of this and be able to disable these artifacts.
///
/// Any block operations on the block device may reject the given buffer has incorrect geometry.
trait SysFsBlockDevice: Sync + Send + BlockDev {
    /// Disables all artifacts for this device in the SysFs
    fn disable(&self);

    /// Returns the unique id for this device
    fn get_id(&self) -> BlockDeviceId;

    fn as_any(self: &mut alloc::boxed::Box<Self>) -> &mut Box<dyn Any>;
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
struct BlockDeviceId {
    class: alloc::boxed::Box<dyn BlockDevClass>, // this should be a zero pointer anyway
    dev_num: BlockDevCounter,
}

enum BlockDevIoErr {
    Misaligned,
    OutOfRange,
}

impl BlockDeviceId {
    pub fn new<C: BlockDevClass>(class: C) -> Self {
        Self {
            class,
            dev_num: class.alloc_id(),
        }
    }

    pub fn token(&self) -> String {
        let mut t = self.class.get_token().to_string();
        t += "-";
        t += &*into_alphas(self.dev_num);
        t
    }
}

fn into_alphas(mut num: BlockDevCounter) -> String {
    const BASE: BlockDevCounter = 26;
    let mut buff = String::with_capacity(num.div_ceil(26) as usize);

    while num > BASE {
        buff += (&into_alpha(num % BASE)) as &str;
        num /= BASE
    }

    buff
}

/// Helper function to convert integer into char
fn into_alpha(num: BlockDevCounter) -> char {
    debug_assert!(num < 26);
    let mut index = 0x61; // ASCII "a"

    index + BlockDevCounter;

    char::from(index)
}

/// Trait for a block device. This is an interface for reading and writing blocks of data of a
/// predefined size.
trait BlockDev {
    /// Reads `T` from the device.  Implementations may return Err(_) if the size of `T` is not
    /// aligned to [Self::get_bs]
    async fn read<T>(
        &self,
        seek: BlockDevGeom,
        size: usize,
    ) -> Result<alloc::boxed::Box<T>, BlockDevIoErr>
    where
        T: Sized;

    /// Writes the given buffer onto the device. The buffer size should be aligned to the devices
    /// block size, this fn may return Err(_) if it is not. A reference to the buffer is returned
    async unsafe fn write<T>(
        &self,
        seek: BlockDevGeom,
        buff: core::pin::Pin<&'static mut T>,
    ) -> Result<(), BlockDevIoErr>;

    /// Returns a struct containing the geometry of the device.
    /// The device geometry must include the block size of the device and the number of blocks in
    /// the device.
    fn geom<T>(&self) -> T;
}

/// A buffer wrapper for a DMA operation. To allow the caller of [BlockDev::write] to reclaim the
/// buffer after the operation has concluded
#[derive(Clone)]
pub struct BlockWriteBuffer<T> {
    arc: alloc::sync::Arc<T>,
}

impl<T> BlockWriteBuffer<T> {
    pub fn decompose(self) -> Result<alloc::boxed::Box<T>, Self> {}
}

impl<T, U: Into<Arc<T>>> From<U> for BlockWriteBuffer<T> {
    fn from(value: U) -> Self {
        Self { arc: value.into() }
    }
}

impl<T> Deref for BlockWriteBuffer<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &*self.arc
    }
}

/// This trait is for a type used to identify a block device type, It is used dynamically within
/// [BlockDeviceId]. It is recommended that it should be a ZST.
trait BlockDevClass: Debug + Copy + Eq + Ord {
    /// Returns a token uniquely identifying the block devices class. i.e. SATA, SCSI, USB
    fn get_token(&self) -> &'static str;

    /// Allocates a unique id number for the given class.
    fn alloc_id(&self) -> BlockDevCounter;

    /// Frees an id to be reused
    ///
    /// Note: This does not need to do anything.
    /// Implementations may allocate by checking existing ids
    fn free_id(&self, id: BlockDevCounter);
}
