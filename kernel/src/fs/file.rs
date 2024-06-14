//! This module contains types used for filesystem implementations and guidelines for those implementations.
//! The filesystem is intended to be posix compatible at the user level which guides the design of
//! traits within this module.
//!
//! Within the kernel files are represented as "file objects" (`dyn File`).
//! These file objects should actually be references to the drivers "file accessor" which should be
//! unique for each opened file on the system. File accessors must be owned by driver.
//! File accessors have few limits imposed for them, namely that only one file accessor can exist for any unique file.
//! Apart from this they may do whatever they like as long as they act as expected when accessed by the file object.
//!
//! Normally UNIX systems uniquely identify each file in the filesystem with an "inode" number,
//! in Hootux this number is not strongly defined. A filesystem is free to completely omit inode
//! numbers and generate them on the fly with a few restrictions.
//!
//! * The inode numbers in a directory must be unique for each "uinique" file within a directory.
//! * Hard links would be considered not unique and must have the same inode number.
//! * A file and a reflink to that file would be considered unique files.
//!
//! Inode numbers do not need to be unique within a filesystem, just a directory as long as the file accessor is open.
//!
//! Normally unix systems use a major and minor number to uniquely identify each filesystem.
//! While this does need to be implemented for compatibility reasons I'll probably emulate this
//! behaviour somehow to prevent the filesystem driver from needing to use this.
//! With all our modern technology we better ways of uniquely identifying a filesystem.

use core::fmt::Formatter;
use core::future::Future;
use crate::fs::{IoError, IoResult};

/// Represents a file. This trait should not actually be implemented on filesystem "file" primitives,
/// instead implementors should act more like a file descriptor where the implementor is
/// used to access the actual file within a filesystem.
///
/// The type `T` represents the character that the file uses. Typically, this should always be `u8`
/// however I see no point in forcing this. Most kernel traits and structures will require that this is `u8`.
/// When `T` is not `u8` the actual character should be allowed to be cast to `[u8]` and back.
///
/// All files must act *in a way* the same. They must all *act* as a normal file when being used by user software.
/// Kernel defined file types all have blanket impls for [`NormalFile<u8>`],
/// so files may only implement one major file type trait.
// todo normal file blanket impls
pub trait File: Send + Sync {
    /// Upcasts `self` into either a [NormalFile] or [Directory].
    ///
    /// If a filesystem implementation defines a file which is not otherwise defined by the kernel it should
    /// return [FileTypeWrapper::NativeFile]. This may be used to provide special implemented by the filesystem.
    /// A private File impl may leak into the kernel however they may not leak into user mode.
    /// A filesystem may also [FileTypeWrapper::NativeFile] this to return public custom File impls with unique APIs.
    fn downcast(self: alloc::boxed::Box<Self>) -> FileTypeWrapper;

    fn file_type(&self) -> IoResult<FileType>;

    /// Returns the optimal block size for this
    fn block_size(&self) -> u64;

    /// Creates a new file object accessing the same file accessor.
    ///
    /// If the file has a cursor the returned file object will have its cursor set to `0`.
    /// If `self` holds a file lock the returned file will not be able to perform file operations
    /// until the lock is freed.
    fn clone_file(&self) -> alloc::boxed::Box<dyn File>;
}

/// Represents a normal file which can be read and written has a cursor which is kept by the file object.
/// A normal file
pub trait NormalFile<T>: Read<T> + Write<T> + Seek + File + Send + Sync {
    /// Returns the size of the file in chars
    ///
    /// Using `core::mem::size_of::<T>() * NormalFile::len(file)` as usize will return the size in bytes
    fn len(&self) -> IoResult<u64>;

    /// Exclusively locks the file, any read or write calls must return [IoError::Exclusive]
    ///
    /// A file must be unlocked by calling [Self::unlock]
    ///
    /// ## Implementation notes
    ///
    /// This works by giving the file accessor a [alloc::sync::Weak] `lock` containing the file object.
    /// When the file is accessed the accessor checks if the `lock` is `Some(_)` if it is then the accessor is locked.
    /// If the file is locked it must then compare the reference contained within the lock with the
    /// file object being used for the operation. If they have the same address then that is the
    /// file object with permission to use IO on the file.
    fn file_lock<'a>(self: alloc::boxed::Box<Self>) ->  futures_util::future::BoxFuture<'a, Result<LockedFile<T>,(IoError, alloc::boxed::Box<dyn NormalFile<T>>)>>;

    /// Unlocks self.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that they actually own the lock to self.
    ///
    /// # Errors
    ///
    /// This function will return [IoError::Exclusive] when `self` is not locked.
    ///
    /// Note: The file may not be deleted from the filesystem while it is locked.
    unsafe fn unlock_unsafe(&self) -> IoResult<()>;
}

pub trait Read<T> {
    /// Reads the data at the cursor into `buff` returning a slice containing the data read.
    /// The returned buffer will only contain data which was read, the returned slice may be smaller than `buff`
    /// This will advance the cursor by the `len()` of the returned buffer.
    ///
    /// This method takes a `&mut self` because the cursor may need to be modified.
    /// Between when this fn is called and when it returns the state of the cursor is not defined.
    ///
    /// # Errors
    ///
    /// If this fn returns `Err(_)` it will return the number of characters read before it encountered an error.
    ///
    /// [IoError::NotPresent] - Will be returned when the file no longer exists.
    fn read<'a>(&'a mut self, buff: &'a mut [T]) -> futures_util::future::BoxFuture<Result<&'a mut [T],(IoError,usize)>>;
}

pub trait Write<T> {
    /// Writes the buffer into the file, at the cursor overwriting any data present.
    /// Advances the cursor by the returned value.
    ///
    /// We can return a `usize` here because a single op cannot reasonably write more than `usize::MAX` bytes
    fn write<'a>(&'a mut self, buff: &'a [T]) -> IoResult<usize>;
}


pub trait Seek {

    /// Moves the cursor by `seek`. Returns the new cursor position.
    fn move_cursor(&mut self, seek: i64) -> Result<u64, IoError> {
        if seek.is_positive() {
            self.seek(seek.abs() as u64)
        } else {
            self.rewind(seek.abs() as u64)
        }
    }

    /// Sets the cursor to the requested position, if an error occurs then the cursor will not be moved.
    /// Returns the new cursor position.
    fn set_cursor(&mut self, pos: u64) -> Result<u64,IoError>;

    /// Moves the cursor back by `pos` characters. Returns the new cursor position.
    fn rewind(&mut self, pos: u64) -> Result<u64,IoError>;

    /// Moves the cursor forward by `pos` chars. Returns the new cursor position.
    fn seek(&mut self, pos: u64) -> Result<u64,IoError>;
}

/// Represents a directory in a filesystem containing files and other directories.
///
/// A directory must be able to be mutably aliased, and any functions using this must expect the
/// state of a directory to change without warning.
///
/// A directory must implement [File] however both read and write functions may return
/// an error immediately. Because of this the type `T` in file doesn't matter.
///
/// A directory must always contain an identifier for its parent (the file "..").
pub trait Directory: File {

    /// Returns the number of entries in this directory.
    /// Because `self` may be aliased this may change without warning.
    fn len(&self) -> IoResult<usize>;

    /// Adds a new file entry into `self`.
    ///
    /// When this fn returns and `file` is `Some(_)` the file at `name` must have identical contents as the given file
    /// When this fn is called and `file` is `None` the directory will create a new empty file.
    ///
    /// This may fail as a result of an error with `self` of `file` the `Err` variant this may
    /// return may contain 1 or 2 errors. Each tuple field representing `self` and `file` respectively.
    fn new_file<'a>(&'a self, name: &'a str, file: Option<&'a mut dyn NormalFile<u8>>) -> futures_util::future::BoxFuture<Result<(), (Option<IoError>, Option<IoError>)>>;

    /// Constructs a new directory, automatically returns a file object for the new directory.
    // If we are making a new dir we probably also want to use it. This optimizes out the extra fetch.
    fn new_dir<'a>(&'a self, name: &'a str) -> IoResult<alloc::boxed::Box<dyn Directory>>;

    /// Requests a file handle from the directory.
    ///
    /// Returns `None` if no file matching `name` exists.
    ///
    /// If self is the root of the filesystem and `name` is `..` then the returned file should contain self.
    fn get_file<'a>(&'a self, name: &'a str) -> IoResult<Option<alloc::boxed::Box<dyn File>>>;

    /// Returns a vec containing all entries within the directory.
    ///
    /// The returned names are not required to be in any particular order.
    ///
    /// This must always return "." and ".." files
    // this may as well be a vec because we need to allocate space for the strings too
    // In theory the FS should cache the dir, but it may not so thi needs to be a future
    // todo optimize this to return a single buffer
    fn file_list(&self) -> IoResult<alloc::vec::Vec<alloc::string::String>>;
}

/// Trait for a base filesystem. A filesystem must implement this to be included in the VFS.
pub trait Filesystem {
    /// Return the root directory of the filesystem.
    fn root(&self) -> alloc::boxed::Box<dyn Directory>;
}

/// Downcasts a [File] object into its lower level implementation.
/// This returns the resulting type in a `Result`, this allows the caller to handle the file not
/// being the expected type, instead of just panicking.
///
/// This is intended to be used for a cloned file object.
///
/// ``` ignore
/// extern crate alloc;
///
/// fn example(file: &alloc::boxed::Box<dyn hootux::fs::file::NormalFile<u8>>) -> alloc::boxed::Box<dyn hootux::fs::file::NormalFile<u8>> {
///     hootux::fs::file::downcast_file!(file.clone_file(),Normal).unwrap()
/// }
/// ```
///
/// Note that the second argument is a [FileTypeWrapper] variant not the trait name.
#[macro_export]
macro_rules! downcast_file {
    ($obj:expr, $ty:ident) => {
        match $crate::fs::file::File::downcast($obj) {
            $crate::fs::file::FileTypeWrapper::$ty(file) => Ok(file),
            other => Err(other)
        }
    };
}

pub use downcast_file;

#[derive(Copy, Clone, Debug)]
pub struct FileMetadata {
    /// File size in bytes
    ///
    /// For directories this may always read `0` but should *try* to contain the actual size of the dir
    size: u64,
    /// Recommended block size. For optimal performance blocks should use this
    /// size and alignment to access the file
    block_size: core::num::NonZeroU64,
    f_type: FileType,
}

impl FileMetadata {

    /// Constructs a new instance of self
    ///
    /// # Panics
    ///
    /// This fn will panic if `block_size` is zero.
    pub fn new(size: u64, block_size: u64, file_type: FileType) -> Self {
        Self {
            size,
            block_size: block_size.try_into().expect("Tried to create a FileMetadata with block size of `0`"),
            f_type: file_type
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn block_size(&self) -> u64 {
        self.block_size.get()
    }

    pub fn file_type(&self) -> FileType {
        self.f_type
    }
}


#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FileType {
    NormalFile,
    Directory,
    CharDev,
    BlkDev,
}


/// Returned by [File::downcast], Contains a downcast file.
///
/// This implements [core::fmt::Debug] and will print the variant name.
pub enum FileTypeWrapper {
    Normal(alloc::boxed::Box<dyn NormalFile<u8>>),
    Dir(alloc::boxed::Box<dyn Directory>),

    /// This file type is native to this particular filesystem and provides its own interface for
    /// interacting with it.
    ///
    /// This type of file *may* not have a public interface at all,
    /// If this is the case then this file may not be intended to be publicly accessible.
    NativeFile(alloc::boxed::Box<dyn File>),
}

impl core::fmt::Debug for FileTypeWrapper {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            FileTypeWrapper::Normal(_) => f.write_str("Normal"),
            FileTypeWrapper::Dir(_) => f.write_str("Dir"),
            FileTypeWrapper::NativeFile(_) => f.write_str("NativeFile"),
        }
    }
}

/// Wrapper for a file with a lock.
///
/// The caller should call [LockedFile::unlock] to drop an instance of `Self`.
/// Otherwise, calling [Drop::drop] will block execution on this thread until the file lock can be
/// released properly.
// todo: allow clones of this. Requires re-working the drop implementations.
pub struct LockedFile<T> {
    file: crate::util::SingleArc<dyn NormalFile<T>>,
}

impl<T> LockedFile<T> {

    pub fn new_from_lock(file: crate::util::SingleArc<dyn NormalFile<T>>) -> Self {
        Self {
            file,
        }
    }

    pub async fn unlock(self) -> Result<alloc::boxed::Box<dyn NormalFile<T>>,IoError> {
        // SAFETY: Aren't we forgetting one teensy-weenzy, but ever so crucial little tiny detail
        unsafe { self.file.unlock_unsafe().await? };

        // absofuckinglutely do not call drop
        // This is to avoid E0509
        // SAFETY: We forget self.file so its destructor is never called.
        let file = unsafe { core::ptr::read(&self.file) };
        core::mem::forget(self);
        Ok(file.take())
    }
}

// SAFETY: I cannot for the life of me figure out why this isn't Send + Sync anyway.
// todo figure out why
unsafe impl<T> Send for LockedFile<T> {}
unsafe impl<T> Sync for LockedFile<T> {}

// fixme: Watch AsyncDrop trait.
impl<T> Drop for LockedFile<T> {
    fn drop(&mut self) {

        // awful one-liner.
        // polls unlock with a dummy waker until it's ready.
        while unsafe {let mut p = core::pin::pin!(self.file.unlock_unsafe());
            p.as_mut().poll(&mut core::task::Context::from_waker(futures_util::task::noop_waker_ref()))}.is_pending() {}
    }
}

impl<T> core::ops::Deref for LockedFile<T> {
    type Target = dyn NormalFile<T>;
    fn deref(&self) -> &Self::Target {
        &*self.file
    }
}

impl<T> core::ops::DerefMut for LockedFile<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.file
    }
}
