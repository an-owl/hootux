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
use crate::mem::dma::DmaBuff;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use cast_trait_object::DynCastExt;
use core::any::Any;
use core::pin::Pin;
use core::task::{Context, Poll};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;

pub mod bus;
pub mod firmware;

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
    pub bus: bus::SysfsBus,                    // inode 1
    pub firmware: firmware::FirmwareContainer, // inode 2
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
        unsafe {
            let dupe = Self {
                bus: bus::SysfsBus::init(),
                firmware: firmware::FirmwareContainer::new(),
            };
            assert!(
                core::ptr::replace(&raw mut SYSFS_ROOT, Some(dupe.clone()),).is_none(),
                "SYSFS_ROOT already initialized"
            );

            hootux::block_on!(hootux::fs::get_vfs().mount(
                Box::new(dupe),
                "/sys",
                hootux::fs::vfs::MountFlags::empty(),
                "",
            ))
            .expect("Failed to mount sysfs");
        }
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

// This relies on a bit of fuckery
pub(crate) fn clone_sysfs_file(origin: &(dyn SysfsFile + 'static)) -> Box<dyn SysfsFile + 'static> {
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
    fn entries(&self) -> IoResult<'_, usize> {
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

    fn file_list(&self) -> IoResult<'_, Vec<String>> {
        async { Ok(SysfsDirectory::file_list(self)) }.boxed()
    }

    fn remove<'f, 'a: 'f, 'b: 'f>(&'a self, name: &'b str) -> IoResult<'f, ()> {
        async { SysfsDirectory::remove(self, name) }.boxed()
    }
}

/// This file is used to notify tasks when an event within the sysfs occurs.
///
/// When either [Read::read] is called this file will return only after an event occurs and the
/// function will return no data.
///
/// sysfs modules that use this file must call [Self::notify_event] in order to notify waiting tasks
/// of an event. [Self::notify_event] is exported via [File::method_call].
#[file]
#[derive(Clone)]
pub struct EventFile {
    slaves: alloc::sync::Arc<spin::Mutex<Vec<alloc::sync::Arc<EventWait>>>>,
}

impl EventFile {
    fn new() -> Self {
        Self {
            slaves: alloc::sync::Arc::new(spin::Mutex::new(vec![])),
        }
    }

    fn notify_event(&self) {
        let slaves = core::mem::replace(&mut *self.slaves.lock(), Vec::new());
        for slave in slaves {
            slave.ready()
        }
    }
}

impl File for EventFile {
    fn file_type(&self) -> FileType {
        FileType::NormalFile
    }

    fn block_size(&self) -> u64 {
        1
    }

    fn device(&self) -> DevID {
        DevID::new(crate::fs::vfs::MajorNum::SYSFS_NUM, 0)
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        u64::MAX
    }

    fn len(&self) -> IoResult<'_, u64> {
        async { Ok(0) }.boxed()
    }

    fn method_call<'f, 'a: 'f, 'b: 'f>(
        &'b self,
        method: &str,
        arguments: &'a (dyn Any + Send + Sync + 'static),
    ) -> IoResult<'f, MethodRc> {
        impl_method_call!(method, arguments => async notify_event())
    }
}

impl SysfsFile for EventFile {}

impl NormalFile for EventFile {
    fn len_chars(&self) -> IoResult<'_, u64> {
        async { Ok(0) }.boxed()
    }

    fn file_lock<'a>(
        self: Box<Self>,
    ) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
        async { Err((IoError::NotSupported, self as Box<dyn NormalFile<u8>>)) }.boxed()
    }

    unsafe fn unlock_unsafe(&self) -> IoResult<'_, ()> {
        async { Err(IoError::Exclusive) }.boxed()
    }
}

impl Write<u8> for EventFile {
    fn write(
        &self,
        _: u64,
        buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        self.read(0, buff) // read will yield the same return value as write
    }
}

impl Read<u8> for EventFile {
    fn read(
        &self,
        _: u64,
        buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async {
            let event_fut = alloc::sync::Arc::new(EventWait::new());
            self.slaves.lock().push(event_fut.clone());
            EventWaitNewType(event_fut).await;
            Ok((buff, 0))
        }
        .boxed()
    }
}

struct EventWait {
    ready: core::sync::atomic::AtomicBool,
    waker: futures_util::task::AtomicWaker,
}

impl EventWait {
    const fn new() -> Self {
        Self {
            ready: core::sync::atomic::AtomicBool::new(false),
            waker: futures_util::task::AtomicWaker::new(),
        }
    }

    fn ready(&self) {
        self.ready.store(true, atomic::Ordering::Relaxed); // this might be fine as relaxed?
        core::sync::atomic::fence(atomic::Ordering::Release);
        self.waker.wake();
    }
}

impl Future for EventWaitNewType {
    type Output = (); // we never ever return error

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.0.ready.load(atomic::Ordering::Acquire) {
            true => Poll::Ready(()),
            false => {
                self.0.waker.register(cx.waker());
                Poll::Pending
            }
        }
    }
}

struct EventWaitNewType(alloc::sync::Arc<EventWait>);

struct BindingFileAccessor {
    bound: hootux::util::Weak<dyn NormalFile>,
    serial: usize,
}

#[file]
#[derive(Clone)]
pub struct BindingFile {
    acc: alloc::sync::Arc<BindingFileAccessor>,
}

impl SysfsFile for BindingFile {}

impl BindingFile {
    pub fn new() -> Self {
        static SERIAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        Self {
            acc: alloc::sync::Arc::new(BindingFileAccessor {
                bound: Default::default(),
                serial: SERIAL.fetch_add(1, atomic::Ordering::Relaxed),
            }),
        }
    }
}

impl File for BindingFile {
    fn file_type(&self) -> FileType {
        FileType::NormalFile
    }
    fn block_size(&self) -> u64 {
        1
    }
    fn device(&self) -> DevID {
        DevID::new(hootux::fs::vfs::MajorNum::SYSFS_NUM, 0)
    }
    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }
    fn id(&self) -> u64 {
        self.acc.serial as u64
    }
    fn len(&self) -> IoResult<'_, u64> {
        async { Ok(0) }.boxed()
    }
}

impl Read<u8> for BindingFile {
    fn read(
        &self,
        _pos: u64,
        buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async { Ok((buff, 0)) }.boxed()
    }
}

impl Write<u8> for BindingFile {
    fn write(
        &self,
        _pos: u64,
        buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async { Ok((buff, 0)) }.boxed()
    }
}

impl NormalFile for BindingFile {
    fn len_chars(&self) -> IoResult<'_, u64> {
        self.len()
    }

    fn file_lock<'a>(
        self: Box<Self>,
    ) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
        async {
            let op_file = self.as_ref().clone();
            let strong = hootux::util::SingleArc::new(self as _);
            if let Ok(()) = op_file.acc.bound.set(&strong) {
                Ok(LockedFile::new_from_lock(strong))
            } else {
                Err((IoError::Exclusive, strong.take()))
            }
        }
        .boxed()
    }

    unsafe fn unlock_unsafe(&self) -> IoResult<'_, ()> {
        async {
            self.acc.bound.clear();
            Ok(())
        }
        .boxed()
    }
}
