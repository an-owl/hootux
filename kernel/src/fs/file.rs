//! This module contains types used for filesystem implementations and guidelines for those implementations.
//! The filesystem is intended to be posix compatible at the user level which guides the design of
//! traits within this module.
//!
//! ## File traits
//!
//! Within the kernel files are represented as "file objects" (`dyn File`).
//! These file objects should actually be references to the drivers "file accessor" which should be
//! unique for each opened file on the system. File accessors must be owned by driver.
//! File accessors have few limits imposed for them, namely that only one file accessor can exist for any unique file.
//! Apart from this they may do whatever they like as long as they act as expected when accessed by the file object.
//!
//! Unlike within a normal Unix-like filesystem which must explicitly open a file.
//! The Hootux filesystem within the kernel implicitly opens files when they are accessed from their parents.
//!
//! ## Device files
//!
//! Special files must have an associated [DevID]. This is used by the kernel to uniquely identify device files.
//! A [DevID] can be combined by to identify a file within a filesystem.
//! These capabilities are used by the VFS to enable cache lookups, and to provide capabilities to
//! drivers which they may not be able to implement themselves.
//!
//! ## File casting
//!
//! Files are normally as a [File] however this is not necessarily a useful type files mut be
//! downcast into other types for this. This module provides the macro [cast_file], which is the
//! preferred method for casting files.

/*
    TODO: Currently this is a big important todo. The file interface has a massive use-after-free bug in it now.
    This is because it uses &'a {mut} [T] Instead of taking ownership or being 'static
    The caller can free memory and then the file will be using uninitialized memory for IO-ops.
    To fix this we need to allow explicit double-frees. Do this by using a bit in the PTE to mark
    the page as having extra metadata within a map. Any writes must be COW and frees must act as
    reference counted.
    Until this is done core::mem::forget()-ing any futures may cause UB.
*/

use crate::fs::{IoError, IoResult};
use crate::mem::dma::DmaBuff;
use alloc::string::ToString;
use cast_trait_object::DynCast;
use core::fmt::Formatter;
use core::future::Future;
use futures_util::FutureExt;
pub use kernel_proc_macro::file;
pub use kernel_proc_macro::impl_method_call;

self::file_derive_debug!(File);

cast_trait_object::create_dyn_cast_config!(pub CastFileToFile = File => File);
cast_trait_object::create_dyn_cast_config!(pub CastFileToNormalFile = File => NormalFile<u8>);
cast_trait_object::create_dyn_cast_config!(pub CastFileToDirectory = File => Directory);
cast_trait_object::create_dyn_cast_config!(pub CastFileToFilesystem = File => super::device::FileSystem);
cast_trait_object::create_dyn_cast_config!(pub CastFileToFifo = File => super::device::Fifo<u8>);
cast_trait_object::create_dyn_cast_config!(pub CastFileToDevice = File => super::device::DeviceFile);

/// Represents a file. This trait should not actually be implemented on filesystem "file" primitives,
/// instead implementors should act more like a file descriptor where the implementor is
/// used to access the actual file within a filesystem.
///
/// The type `T` represents the character that the file uses. Typically, this should always be `u8`
/// however I see no point in forcing this. Most kernel traits and structures will require that this is `u8`.
/// When `T` is not `u8` the actual character should be allowed to be cast to `[u8]` and back.
///
/// A file may require that previous asynchronous calls are polled to completion before calling a new one.
/// If this is the case then the function should return [super::IoError::Busy].
/// Implementations should make a best effort to avoid this behaviour where it is reasonable.
pub trait File:
    Send
    + Sync
    + DynCast<CastFileToFile>
    + DynCast<CastFileToNormalFile>
    + DynCast<CastFileToDirectory>
    + DynCast<CastFileToFilesystem>
    + DynCast<CastFileToFifo>
    + DynCast<CastFileToDevice>
{
    /*
    /// Upcasts `self` into either a [NormalFile] or [Directory].
    ///
    /// If a filesystem implementation defines a file which is not otherwise defined by the kernel it should
    /// return [FileTypeWrapper::NativeFile]. This may be used to provide special implemented by the filesystem.
    /// A private File impl may leak into the kernel however they may not leak into user mode.
    /// A filesystem may also [FileTypeWrapper::NativeFile] this to return public custom File impls with unique APIs.
    fn upcast(self: alloc::boxed::Box<Self>) -> FileTypeWrapper;

     */

    fn file_type(&self) -> FileType;

    /// Returns the optimal block size for this
    fn block_size(&self) -> u64;

    /// Returns the device ID owner for the file.
    ///
    /// Device files may never return Err(_)
    fn device(&self) -> DevID;

    /// Creates a new file object accessing the same file accessor.
    ///
    /// If the file has a cursor the returned file object will have its cursor set to `0`.
    /// If `self` holds a file lock the returned file will not be able to perform file operations
    /// until the lock is freed.
    fn clone_file(&self) -> alloc::boxed::Box<dyn File>;

    /// Returns a file ID.
    ///
    /// This ID is used by the VFS for looking up cached data.
    ///
    /// - When this filesystem does not support caching or device files the return value of this is undefined.
    /// - When this filesystem supports caching all files within the filesystem must have a unique ID
    /// - When this filesystem does not support caching but supports device files all directories must have a unique ID, other files are undefined.
    /// - When `self` is a device file then the value of ID is undefined.
    fn id(&self) -> u64;

    /// Returns the size of a file.
    ///
    /// - If `self` is a [NormalFile] this must return the same value as the result of [NormalFile::len_chars].
    /// - If `self` is a [Directory] then this should return the number of directories contained within self.
    /// - If `self` is any other type then this fn should hint the number of characters expected to be read.
    /// - If the implementation cannot provide any hint then this may return any non-zero value.
    fn len(&self) -> IoResult<u64>;

    /// Returns a B-side file if one exists with the given arguments.
    /// All valid IDs should be documented in the implementation documentation.
    ///
    /// A file may want to return multiple B-side files with the same character (like [u8]).
    /// For this reason the `id` argument may be given. An implementation of this function that does
    /// not require this field may ignore it, however a caller must set it to `0` for forwards compatibility reasons.
    ///
    /// B-side file are allowed to omit cursors when appropriate, in this case the cursor should
    /// remain at `0` attempting to move it should return [IoError::EndOfFile].
    ///
    /// Calling [File::device] on a B-side file must return the same value as the A-side file.
    /// [File::file_type] must return [FileType::NormalFile].
    /// All other metadata values are undefined.
    ///
    /// Writing to a B-side file may have side effects for the A-side as long as these effects are
    /// transparent to the operation of the file. All other effects are not permitted,
    ///
    /// Calling [File::b_file] on a B-side file is not permitted
    // todo distinguish b-files, maybe add UUID for file types to distinguish them?
    fn b_file(&self, _id: u64) -> Option<alloc::boxed::Box<dyn File>> {
        None
    }

    /// This method allows implementations to export methods which are not provided by the file traits.
    ///
    /// This is done by matching `method` against the method name and downcasting `argumenta` into
    /// their concrete types.
    /// As you may expect this has limitations, any methods must take self as `&self` and all
    /// arguments must be taken as a reference.
    ///
    /// When a method takes no arguments a reference to the unit type should be given.
    ///
    /// The return value of the method will be cast into a [MethodRc] which will contain a
    /// `Box<dyn Any>` which can be downcast into the expected return type.
    ///
    /// Implementations must return either `Ok(future)` or `Err(IoError::NotPresent)`
    ///
    /// ## Implementation notes
    ///
    /// A macro [impl_method_call] is provided to implement this method.
    /// It's syntax is `$method_arg:ident, $method_args:ident => $( $(async)? $method_name:ident($($method_arg:ty),*))`
    /// This will match `$method_arg` to determine which method will be called, downcast `$method_args`
    /// into their concrete types and call `Self::$method_name` with the downcast arguments.
    ///
    /// Methods must be async, of they arent specifying `async $method_name..` will place the method in an async block.
    #[warn(unused_variables)]
    fn method_call<'f, 'a: 'f, 'b: 'f>(
        &'b self,
        method: &str,
        arguments: &'a (dyn core::any::Any + Send + Sync + 'a),
    ) -> IoResult<'f, MethodRc> {
        async { Err(IoError::NotPresent) }.boxed()
    }
}

self::file_derive_debug!(NormalFile<u8>);

/// Represents a normal file which can be read and written has a cursor which is kept by the file object.
/// A normal file.
///
/// Implementations with characters other than [u8] are not permitted to accessible from userspace.
///
/// NormalFile requires both the [Read] and [Write] traits, these take `self` as an *immutable*
/// reference, however these operations necessarily require mutating the data. All operations from
/// a single file-object must be strongly ordered.
/// Operations from different file-objects may be dynamically ordered.
///
/// See [File::b_file] for documentation when the implementor is a B-side file
pub trait NormalFile<T = u8>: Read<T> + Write<T> + File + Send + Sync {
    /// Returns the size of the file in chars
    ///
    /// Using `core::mem::size_of::<T>() * NormalFile::len(file)` as usize will return the size in bytes
    fn len_chars(&self) -> IoResult<u64>;

    /// Exclusively locks the file, any read or write calls must return [IoError::Exclusive]
    ///
    /// A file must be unlocked by calling [Self::unlock]
    ///
    /// If a file may not be locked it must return [IoError::NotSupported].
    /// This may occur under certain circumstances such as `self` being a special file.
    ///
    /// ## Implementation notes
    ///
    /// This works by giving the file accessor a [alloc::sync::Weak] `lock` containing the file object.
    /// When the file is accessed the accessor checks if the `lock` is `Some(_)` if it is then the accessor is locked.
    /// If the file is locked it must then compare the reference contained within the lock with the
    /// file object being used for the operation. If they have the same address then that is the
    /// file object with permission to use IO on the file.
    fn file_lock<'a>(
        self: alloc::boxed::Box<Self>,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<LockedFile<T>, (IoError, alloc::boxed::Box<dyn NormalFile<T>>)>,
    >;

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

/// This trait's methods may have side effects. Any side effects should be documented at the implementation level.
pub trait Read<T> {
    /// Reads the data at the cursor into `buff` returning the buffer and the number of bytes read,
    /// this is not required to fill `buff`.
    ///
    /// The implementors may modify any part of the buffer including the trailing bytes after the
    /// returned data.
    ///
    /// # Errors
    ///
    /// If this fn returns `Err(_)` it will return the number of characters read before it encountered an error.
    ///
    /// [IoError::NotPresent] - Will be returned when the file no longer exists.
    fn read<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        pos: u64,
        buff: DmaBuff<'b>,
    ) -> futures_util::future::BoxFuture<
        'f,
        Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>,
    >;
}

/// This trait's methods may have side effects. Any side effects should be documented at the implementation level.
pub trait Write<T> {
    /// Writes the buffer into the file, at the position `pos` overwriting any data present or
    /// appending it to the end of the file.
    ///
    /// The data contained in `buff` may not be modified during this operation.
    ///
    /// We can return a `usize` here because a single op cannot reasonably write more than `usize::MAX` bytes
    fn write<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        pos: u64,
        buff: DmaBuff<'b>,
    ) -> futures_util::future::BoxFuture<
        'f,
        Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>,
    >;
}

self::file_derive_debug!(Directory);

/// Represents a directory in a filesystem containing files and other directories.
///
/// A directory must be able to be mutably aliased, and any functions using this must expect the
/// state of a directory to change without warning.
///
/// A directory must implement [File] however both read and write functions may return
/// an error immediately. Because of this the type `T` in file doesn't matter.
///
/// A directory must always contain an identifier for its parent (the file "..").
///
/// When a directory is the filesystem root both "." and ".." should be the same file.
pub trait Directory: File {
    /// Returns the number of entries in this directory.
    /// Because `self` may be aliased this may change without warning.
    fn entries(&self) -> IoResult<usize>;

    /// Adds a new file entry into `self`.
    ///
    /// When this fn returns and `file` is `Some(_)` the file at `name` must have identical contents as the given file
    /// When this fn is called and `file` is `None` the directory will create a new empty file.
    ///
    /// This may fail as a result of an error with `self` of `file` the `Err` variant this may
    /// return may contain 1 or 2 errors. Each tuple field representing `self` and `file` respectively.
    ///
    /// If `file` needs to be read to be added to the directory the driver must attempt to restore the cursor.
    /// If restoring th cursor throws an error then the cursor should be set to 0
    fn new_file<'f, 'b: 'f, 'a: 'f>(
        &'a self,
        name: &'b str,
        file: Option<&'b mut dyn NormalFile<u8>>,
    ) -> futures_util::future::BoxFuture<'f, Result<(), (Option<IoError>, Option<IoError>)>>;

    /// Constructs a new directory, automatically returns a file object for the new directory.
    // If we are making a new dir we probably also want to use it. This optimizes out the extra fetch.
    fn new_dir<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        name: &'b str,
    ) -> IoResult<'f, alloc::boxed::Box<dyn Directory>>;

    /// Stores a file, without modifying its primitive type in the current filesystem.
    /// If a file of the type already exists with `name` it must be shadowed and restored when `file` is removed.
    ///
    /// This method is intended to be used by the VFS when attaching device files to the filesystem
    /// and should not be used for normal files or directories.
    ///
    /// If the filesystem is not capable of storing its own device files then it must return [IoError::NotSupported].
    /// If the filesystem allows caching then the VFS will cache the directory and handle device
    /// files transparently to the filesystem.
    #[allow(unused_variables)]
    fn store<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        name: &'b str,
        file: alloc::boxed::Box<dyn File>,
    ) -> IoResult<'f, ()> {
        async { Err(IoError::NotSupported) }.boxed()
    }

    /// Requests a file handle from the directory.
    ///
    /// Returns `None` if no file matching `name` exists.
    ///
    /// If `self` is the root of the filesystem and `name` is `..` then this must return [IoError::IsDevice].
    ///
    /// The VFS implementation prevents a filename from ever containing the file separator character '/'.
    /// A filesystem may use this character internally to denote internally used files like "/journal"
    fn get_file<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        name: &'b str,
    ) -> IoResult<'f, alloc::boxed::Box<dyn File>>;

    /// Returns a files metadata.
    ///
    /// See [Directory::get_file] for info
    ///
    /// If `name` is a device file where the driver does not know the state of the device then the
    /// may assume its metadata. This method should never return [IoError::IsDevice].
    fn get_file_meta<'f, 'a: 'f, 'b: 'f>(&'a self, name: &'b str) -> IoResult<'f, FileMetadata> {
        async { FileMetadata::new_from_file(&*self.get_file(name).await?).await }.boxed()
    }

    /// Returns information alongside the requested file.
    ///
    /// This is intended to be an optimization over other methods for getting file information,
    /// to prevent multiple lookups when accessing a directory entry.
    ///
    /// When `self` is the root of the filesystem and `name` is ".." then the returned handle must
    /// not contain an accessible file and must indicate that the file is a device.
    ///
    /// The default implementation should not be used. It is very poorly optimized.
    ///
    /// See [Directory::get_file] for more info.
    fn get_file_with_meta<'f, 'a: 'f, 'b: 'f>(&'a self, name: &'b str) -> IoResult<'f, FileHandle> {
        async {
            let meta = self.get_file_meta(name).await?;

            let file = {
                match self.get_file(name).await {
                    Ok(f) => f,
                    Err(IoError::IsDevice) => {
                        return Ok(FileHandle::new_dev(FileMetadata::new_unknown()));
                    }
                    Err(e) => return Err(e),
                }
            };

            Ok(FileHandle::new(file, true, meta))
        }
        .boxed()
    }

    /// Returns a vec containing all entries within the directory.
    ///
    /// The returned names are not required to be in any particular order.
    ///
    /// This must always return "." and ".." files
    // this may as well be a vec because we need to allocate space for the strings too
    // In theory the FS should cache the dir, but it may not so thi needs to be a future
    // todo optimize this to return a single buffer
    fn file_list(&self) -> IoResult<alloc::vec::Vec<alloc::string::String>>;

    /// Removes the file `name` from the directory.
    ///
    /// If `name` is a directory which is not empty, this fn must return [IoError::NotEmpty].
    ///
    /// If `name` is a VFS-managed device file then this fn must remove the entry from the directory
    /// and return [IoError::IsDevice] to inform the VFS to remove the device entry from the device-override list.
    fn remove<'f, 'a: 'f, 'b: 'f>(&'a self, name: &'b str) -> IoResult<'f, ()>;
}

#[macro_export]
macro_rules! cast_file {
    ($ty:path: $id:expr) => {{
        let t: ::core::result::Result<
            ::alloc::boxed::Box<dyn $ty>,
            alloc::boxed::Box<dyn $crate::fs::file::File>,
        > = cast_trait_object::DynCastExt::dyn_cast($id);
        t
    }};
    (& $ty:path: $id:expr) => {{
        let t: ::core::result::Result<&dyn $ty, &dyn $crate::fs::file::File> =
            cast_trait_object::DynCastExt::dyn_cast($id);
        t
    }};
    (mut $ty:path: $id:expr) => {{
        let t: ::core::result::Result<&mut dyn $ty, &mut dyn $crate::fs::file::File> =
            cast_trait_object::DynCastExt::dyn_cast($id);
        t
    }};
}

pub use cast_file;

/// Derives debug for `dyn $tra` when it is wrapped in a Box or Arc
macro_rules! file_derive_debug {
    ($tra:path) => {
        impl ::core::fmt::Debug for ::alloc::boxed::Box<dyn $tra> {
            fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
                write!(f, "Box(File:{:?}({}))", self.file_type(), self.device())
            }
        }
    };
}

pub(crate) use file_derive_debug;

/// Casts a file into a directory regardless of whether it is a filesystem or a directory
// todo integrate this into cast_file
macro_rules! cast_dir {
    ($file:expr) => {
        match cast_file!($crate::fs::file::Directory: $file) {
            Ok(dir) => Ok(dir),
            Err(file) => {
                match cast_file!($crate::fs::device::FileSystem: file) {
                    Ok(fs) => Ok(fs.root()),
                    Err(e) => Err(e),
                }
            }
        }
    };
}

pub(crate) use cast_dir;

#[macro_export]
macro_rules! derive_seek_blank {
    ($($name:ident),*) => {
        $(
        impl $crate::fs::file::Seek for $name {
            fn set_cursor(&mut self, _pos: u64) -> Result<u64,IoError> {
                Err($crate::fs::IoError::NotSupported)
            }

            fn rewind(&mut self, _pos: u64) -> Result<u64,IoError> {
                Err($crate::fs::IoError::NotSupported)
            }

            fn seek(&mut self, _pos: u64) -> Result<u64,IoError> {
                Err($crate::fs::IoError::NotSupported)
            }
        }
        )*
    };
}

pub use derive_seek_blank;

pub use crate::fs::vfs::DevID;

#[derive(Clone, Debug)]
pub struct FileMetadata {
    unknown: bool,

    size: u64,

    block_size: core::num::NonZeroU64,

    id: u64,

    /// Each filesystem has a unique [super::vfs::DevID], this field must contain the ID of the owner of this file.
    /// Normally files are owned by the same filesystem as the directory that contains it,
    /// if the file is a mountpoint or special file this may not be the case.
    device: super::vfs::DevID,
    f_type: FileType,
}

impl FileMetadata {
    /// Constructs a new instance of self
    ///
    /// # Panics
    ///
    /// This fn will panic if `block_size` is zero.
    pub fn new(
        size: u64,
        block_size: u64,
        id: u64,
        device: super::vfs::DevID,
        file_type: FileType,
    ) -> Self {
        Self {
            unknown: false,
            size,
            block_size: block_size
                .try_into()
                .expect("Tried to create a FileMetadata with block size of `0`"),
            id,
            device,
            f_type: file_type,
        }
    }

    /// Constructs an instance of self using the information provided by `file`
    pub async fn new_from_file(file: &dyn File) -> Result<Self, IoError> {
        Ok(Self {
            unknown: false,
            size: file.len().await?,
            block_size: file.block_size().try_into().unwrap(),
            id: file.id(),
            device: file.device(),
            f_type: file.file_type(),
        })
    }

    /// File size in bytes
    ///
    /// For directories this may always read `0` but should *try* to contain the actual size of the dir
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Recommended block size. For optimal performance blocks should use this
    /// size and alignment to access the file
    pub fn block_size(&self) -> u64 {
        self.block_size.get()
    }

    pub fn file_type(&self) -> FileType {
        self.f_type
    }

    /// The file must be given an ID. This ID is used by the kernel for cache lookups.
    ///
    /// For filesystems where caching is allowed the ID for each file in the filesystem must be unique.
    // todo the rest of this
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The device ID of the file. Noted the owner of the file.
    /// When the file is owned by the same system as the filesystem then they must have the same ID.
    /// If the file is a device file then the ID must be the owner of that file, not the directory it was contained within.
    pub fn min_maj(&self) -> DevID {
        self.device
    }

    pub fn new_unknown() -> Self {
        Self {
            unknown: true,
            size: 0,
            block_size: unsafe { core::num::NonZeroU64::new(u64::MAX).unwrap_unchecked() }, // u64::MAX is definitely not 0
            id: 0,
            device: DevID::NULL,
            f_type: FileType::NormalFile,
        }
    }

    pub fn is_unknown(&self) -> bool {
        self.unknown
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FileType {
    NormalFile,
    MountPoint,
    Directory,
    CharDev,
    Native,
    BlkDev,
}

/// Contains a number of filesystem options that can be queried at runtime.
///
/// Options are given as key-value pairs. The values are passed as a [FsOptionVariant] this allows
/// storing integers, booleans or strings.
/// Strings must be copied onto the heap when fetched so the other variants are preferred.
///
/// The value variant for any key be constant, any value that is a bool must always be a bool,
/// it may never switch to an integer.
/// Except for the above rule a driver may define its options however it wants. It the driver wants
/// to use [FsOptionVariant::Int] to store a [char] it may as long as the driver can distinguish between actual values.
/// If the driver wants to store an array it must use [FsOptionVariant::String] instead.
///
/// This struct defines some pre-defined option names. For forward-compatibility reasons drivers
/// **must** use the se const's to access these defined variables. If a driver chooses to not use
/// [FsOpts] to store its options then these options must still be defined by these definitions.
///
/// # Panics
///
/// Attempting to set a predefined option to the incorrect value variant will result in a panic.
#[derive(Debug)]
pub struct FsOpts {
    dir_cache: bool,
    dev_allowed: bool,
    extra: alloc::collections::BTreeMap<alloc::string::String, FsOptionVariant>,
}

impl FsOpts {
    pub const TRUE: FsOptionVariant = FsOptionVariant::Bool(true);
    pub const FALSE: FsOptionVariant = FsOptionVariant::Bool(false);

    /// Indicates whether directories may be cached by the kernel.
    /// A filesystem must disable this is cache synchronization cannot be guaranteed by the driver (i.e. shared filesystems).
    pub const DIR_CACHE: &'static str = "dir_cache";

    /// Indicates that this filesystem may store device files such as pipes, FIFOs or mount points.
    ///
    /// If `dev_allowed` is true the filesystem may implement device file handling natively or via the VFS.
    /// Native device file handling is likely to be faster than VFS device lookups.
    pub const DEV_ALLOWED: &'static str = "dev_allowed";

    /// Constructs a new instance of self.
    ///
    /// This fn is non-exhaustive, other properties may be added in the future.
    pub fn new(dir_cache: bool, dev_allowed: bool) -> Self {
        Self {
            dir_cache,
            dev_allowed,
            extra: Default::default(),
        }
    }

    /// Set a filesystem property.
    ///
    /// This is intended to be used while the filesystem is initialized, not used at runtime.
    #[cold]
    pub fn set<T: Into<FsOptionVariant>>(
        &mut self,
        property: alloc::string::String,
        value: T,
    ) -> &mut Self {
        let value = value.into();
        match &*property {
            Self::DEV_ALLOWED => self.dev_allowed = value.try_into().unwrap(),

            Self::DIR_CACHE => {
                if let FsOptionVariant::Bool(b) = value {
                    self.dir_cache = b
                }
            }
            _ => self
                .extra
                .insert(property, value.into())
                .ok_or(())
                .expect_err("Failed to insert property"),
        }

        self
    }

    /// Returns the value of the requested property if it exists.
    ///
    /// A number of predefined options exist, the names of these are defined as associated constants
    /// these are guaranteed to be present and accessible in O(1).
    /// [Self::TRUE] and [Self::FALSE] which may use for comparisons ([core::ptr::eq] should not be used for comparisons)
    pub fn get(&self, property: &str) -> Option<FsOptionVariant> {
        match property {
            Self::DIR_CACHE => {
                if self.dir_cache {
                    Some(Self::TRUE)
                } else {
                    Some(Self::FALSE)
                }
            }

            Self::DEV_ALLOWED => {
                if self.dev_allowed {
                    Some(Self::TRUE)
                } else {
                    Some(Self::FALSE)
                }
            }

            property => {
                Some(self.extra.get(property)?.clone()) // I hate that this needs `?`
            }
        }
    }
}

/// Helper for wrapping data that may be included within FsOptions
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum FsOptionVariant {
    Int(usize),
    Bool(bool),
    String(alloc::string::String),
}

impl core::str::FromStr for FsOptionVariant {
    type Err = core::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(e) = s.parse() {
            Ok(Self::Bool(e))
        } else if let Ok(e) = s.parse() {
            Ok(Self::Int(e))
        } else {
            Ok(Self::String(s.to_string()))
        }
    }
}

impl core::fmt::Display for FsOptionVariant {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            FsOptionVariant::Int(i) => core::write!(f, "{i}"),
            FsOptionVariant::Bool(b) => core::write!(f, "{b}"),
            FsOptionVariant::String(s) => core::write!(f, "{s}"),
        }
    }
}

impl From<usize> for FsOptionVariant {
    fn from(value: usize) -> Self {
        Self::Int(value.into())
    }
}

impl From<bool> for FsOptionVariant {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<alloc::string::String> for FsOptionVariant {
    fn from(value: alloc::string::String) -> Self {
        Self::String(value)
    }
}

impl TryFrom<FsOptionVariant> for usize {
    type Error = FsOptionVariant;

    fn try_from(value: FsOptionVariant) -> Result<Self, Self::Error> {
        if let FsOptionVariant::Int(i) = value {
            Ok(i)
        } else {
            Err(value)
        }
    }
}

impl TryFrom<FsOptionVariant> for bool {
    type Error = FsOptionVariant;

    fn try_from(value: FsOptionVariant) -> Result<Self, Self::Error> {
        if let FsOptionVariant::Bool(i) = value {
            Ok(i)
        } else {
            Err(value)
        }
    }
}

impl TryFrom<FsOptionVariant> for alloc::string::String {
    type Error = FsOptionVariant;

    fn try_from(value: FsOptionVariant) -> Result<Self, Self::Error> {
        if let FsOptionVariant::String(i) = value {
            Ok(i)
        } else {
            Err(value)
        }
    }
}

impl<'a> TryFrom<&'a FsOptionVariant> for &'a str {
    type Error = &'a FsOptionVariant;

    fn try_from(value: &'a FsOptionVariant) -> Result<Self, Self::Error> {
        if let FsOptionVariant::String(i) = value {
            Ok(&*i)
        } else {
            Err(value)
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
        Self { file }
    }

    pub async fn unlock(self) -> Result<alloc::boxed::Box<dyn NormalFile<T>>, IoError> {
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
        while unsafe {
            let mut p = core::pin::pin!(self.file.unlock_unsafe());
            p.as_mut().poll(&mut core::task::Context::from_waker(
                futures_util::task::noop_waker_ref(),
            ))
        }
        .is_pending()
        {}
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

#[derive(Ord, PartialOrd, Eq, PartialEq)]
pub struct DirId(u64);

impl DirId {
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

/// Required for filesystem file lookup optimization.
/// This is returned when both the file and metadata is required.
pub struct FileHandle {
    pub file_metadata: FileMetadata,
    pub device_hint: bool,
    pub file: Option<alloc::boxed::Box<dyn File>>,
}

impl FileHandle {
    pub fn new(file: alloc::boxed::Box<dyn File>, dev_hint: bool, meta: FileMetadata) -> Self {
        Self {
            file_metadata: meta,
            device_hint: dev_hint,
            file: Some(file),
        }
    }

    /// Returns an instance of self that returns a "device hint" and does not return a file object.
    pub fn new_dev(meta: FileMetadata) -> Self {
        Self {
            file_metadata: meta,
            device_hint: true,
            file: None,
        }
    }

    /// Returns the file contained within self.
    ///
    /// The returned value must always be `Some(_)` if [Self::vfs_device_hint] returns `false`
    pub fn file(self) -> Option<alloc::boxed::Box<dyn File>> {
        self.file
    }

    /// Returns whether this file may be a device file.
    ///
    /// - This **must** return `true` when the file is a device file managed by the kernel VFS.
    /// - This *should* return `true` when the file is a device file managed natively by the filesystem.
    /// - This *may* be `true` when the file is not a device file.
    /// A filesystem much as FAT may always return this as `true`.
    pub fn vfs_device_hint(&self) -> bool {
        self.device_hint
    }

    pub fn metadata(&self) -> FileMetadata {
        self.file_metadata.clone()
    }
}

pub struct MethodRc {
    pub inner: alloc::boxed::Box<dyn core::any::Any>,
}

impl MethodRc {
    pub fn wrap<T: core::any::Any + Send + Sync + 'static>(t: T) -> Self {
        Self {
            inner: alloc::boxed::Box::new(t),
        }
    }
}
