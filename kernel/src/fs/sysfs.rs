//! The sysfs is intended provide an interface to kernel and hardware structures.
//!
//! The sysfs is mounted at `/sys` during kernel initialization.
//!
//! The sysfs inode numbers must be unique. Inode numbers are defined as bitfields,
//! with each field layout being defined by its ancestors.
//!
//! The sysfs is designed to contain side-channels to access files synchronously.
//!
//! All non-directory files are special files regardless of whether they are marked as such.

use super::device::*;
use super::file::*;
use crate::fs::{IoError, IoResult};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use cast_trait_object::DynCastExt;
use futures_util::future::BoxFuture;
use futures_util::FutureExt;

pub mod bus;

static mut SYSFS_ROOT: Option<SysFsRoot> = None;

/// Initialize sysfs. **Must** be called by binary before [hootux::mp::start_mp] is called.
pub fn init() {
    assert!(
        crate::runlevel::runlevel() < crate::runlevel::Runlevel::Kernel,
        "sysfs::init(): Called too late."
    );
    // SAFETY: checking
    unsafe { SysFsRoot::init() };
}

// notes to self.
// Inode numbers should be static for required dirs, then `or` a number on for each subdir

/// File object for the root directory of the sysfs.
/// The root directory uses the first 5-bits of the inode numbers.
///
/// This is a singleton containing the child file objects.
#[derive(Clone)]
#[kernel_proc_macro::file]
#[kernel_proc_macro::impl_sysfs_root_traits]
pub struct SysFsRoot {
    bus: bus::SysfsBus, // inode 1
}

impl SysFsRoot {
    /// Constructs a new SysfsRoot and stores it in [SYSFS_ROOT].
    ///
    /// # Safety
    ///
    /// This fn is unsafe because it is racy. It **must** be called before MP is initialized.
    ///
    /// Note that "must" is not "may".
    ///
    /// # Panics
    ///
    /// This will panic if it has already been called.
    unsafe fn init() {
        core::ptr::replace(
            &raw mut SYSFS_ROOT,
            Some(Self {
                //firmware: SysfsFirmware {},
                bus: bus::SysfsBus::init(),
            }),
        )
        .expect("SYSFS_ROOT already initialized");
    }

    /// Fetches a new SysfsRoot file-object.
    ///
    /// This does not actually construct a new SysfsRoot. See [Self::init]
    pub fn new() -> Self {
        // SAFETY: This fn assumes that SYSFS_ROOT is immutable at this stage.
        unsafe { &mut *&raw mut SYSFS_ROOT }
            .as_ref()
            .unwrap()
            .clone()
    }
}

impl DeviceFile for SysFsRoot {}

/// Trait for files within the sysfs.
///
/// Sysfs-File traits exist to reduce the number of allocations required.
/// But to simplify implementations not all File* methods are implemented
///
/// This trait is mainly intended to allow files to be cast to either a [NormalFile] via [File] or
/// directly to a [SysfsDirectory] via [Self::into_sysfs_dir], which can then be operated on normally.

pub trait SysfsFile: File {
    /// This allows downcasting into a [SysfsDirectory].
    ///
    /// A blanket implementation for [SysfsDirectory] exists to handle casting automatically.
    /// The default implementation will return `None`
    fn into_sysfs_dir(self: Box<Self>) -> Option<Box<dyn SysfsDirectory>> {
        None
    }
}

fn clone_sysfs_file(origin: &(dyn SysfsFile + 'static)) -> Box<dyn SysfsFile + 'static> {
    let c = origin.clone_file();
    let c = Box::leak(c) as *mut dyn File;
    let origin = origin as *const dyn SysfsFile;
    let ret = c.with_metadata_of(origin);
    unsafe { Box::from_raw(ret) }
}

impl<T> SysfsFile for T
where
    T: SysfsDirectory + File + 'static,
{
    fn into_sysfs_dir(self: Box<Self>) -> Option<Box<dyn SysfsDirectory>> {
        Some(self)
    }
}

/// SysfsDirectory is intended to mirror [Directory] using synchronous methods to optimise operation.
///
/// Some functionality from [Directory] is not expected to be used within the sysfs.
/// The sysfs is intended to export data from the kernel and drivers, not to store generic data thus
/// methods for these are not included.
pub trait SysfsDirectory: Directory + File {
    /// Returns the number of files in this directory.
    fn entries(&self) -> usize;

    /// Returns all files accessible within this directory.
    fn file_list(&self) -> Vec<String>;

    /// Return the file `name`. Returns [IoError::NotPresent] when this file is not present.
    fn get_file(&self, name: &str) -> Result<Box<dyn SysfsFile>, IoError>;

    /// Attempt to store `file` with `name` within the directory.
    ///
    /// Implementations may disallow this by returning [IoError::DeviceError]
    fn store(&self, name: &str, file: Box<dyn SysfsFile>) -> Result<(), IoError>;

    /// Remove `name` from the directory.
    ///
    /// Remove `file` implementations may return [IoError::DeviceError] if it does not want to allow this.
    fn remove(&self, name: &str) -> Result<(), IoError>;

    // todo consider moving this the the File impl
    fn as_any(self: Box<Self>) -> Box<dyn core::any::Any>;
}

impl<T> Directory for T
where
    T: SysfsDirectory + File,
{
    fn entries(&self) -> IoResult<usize> {
        async { Ok(SysfsDirectory::entries(self)) }.boxed()
    }

    fn new_file<'f, 'b: 'f, 'a: 'f>(
        &'a self,
        _name: &'b str,
        _file: Option<&'b mut dyn NormalFile<u8>>,
    ) -> BoxFuture<'f, Result<(), (Option<IoError>, Option<IoError>)>> {
        async { Err((Some(IoError::NotSupported), None)) }.boxed()
    }

    fn new_dir<'f, 'a: 'f, 'b: 'f>(&'a self, _name: &'b str) -> IoResult<'f, Box<dyn Directory>> {
        async { Err(IoError::NotSupported) }.boxed()
    }

    #[track_caller]
    fn store<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        _name: &'b str,
        _file: Box<dyn File>,
    ) -> IoResult<'f, ()> {
        // This does not work because we cannot guarantee that `file is a
        log::warn!(
            "{} Called hootux::fs::file::Directory::store() for {}: Illegal, use hootux::fs::sysfs::SysfsDirectory::store() instead",
            core::any::type_name::<Self>(),
            core::panic::Location::caller()
        );
        async { Err(IoError::NotSupported) }.boxed()
    }

    fn get_file<'f, 'a: 'f, 'b: 'f>(&'a self, name: &'b str) -> IoResult<'f, Box<dyn File>> {
        async { SysfsDirectory::get_file(self, name).map(|f| f.dyn_upcast()) }.boxed()
    }

    fn file_list(&self) -> IoResult<Vec<String>> {
        async { Ok(SysfsDirectory::file_list(self)) }.boxed()
    }

    fn remove<'f, 'a: 'f, 'b: 'f>(&'a self, name: &'b str) -> IoResult<'f, ()> {
        async { SysfsDirectory::remove(self, name) }.boxed()
    }
}
