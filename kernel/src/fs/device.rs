use core::fmt::Formatter;
use super::file::*;

/// This is a marker trait for files which act as special device (character/block) files.
///
/// An implementation of this trait should be used to interface with a device, such as a serial port or
/// non-volatile storage.
///
/// Any type that implements this trait must return either [FileType::BlkDev] or [FileType::CharDev] from [File::file_type].
/// Returning any other variant is considered UB.
///
/// When `Self` returns [FileType::CharDev] any data read from or written to the device may not be
/// buffered under any circumstances. When `Self` returns [FileType::BlkDev] any data may be buffered as necessary.
/// Extra data restrictions may also be implemented when operating on the file by driver.
/// The driver must document any IO limitations of the device file and any side effects of reading
/// from and writing to the file. Any other operations are not permitted to have side effects.
///
/// Because a device file may need to be accessed from within the kernel it may not be locked.
/// Attempting to call [NormalFile::file_lock] on a device file must return [super::IoError::NotSupported].
pub trait DeviceFile: File {}

file_derive_debug!(DeviceFile);

pub enum DeviceWrapper {
    Fifo(alloc::boxed::Box<dyn Fifo<u8>>),
    FileSystem(alloc::boxed::Box<dyn FileSystem>)
}

impl core::fmt::Debug for DeviceWrapper {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Fifo(_) => core::write!(f, "Fifo"),
            Self::FileSystem(_) => core::write!(f, "FileSystem"),
        }
    }
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, Default)]
pub enum OpenMode {
    #[default]
    Locked,
    Read,
    Write,
    ReadWrite,
}

impl OpenMode {
    pub fn is_read(&self) -> bool {
        match self {
            Self::Read | Self::ReadWrite => true,
            _ => false
        }
    }

    pub fn is_write(&self) -> bool {
        match self {
            Self::Write | Self::ReadWrite => true,
            _ => false
        }
    }
}

file_derive_debug!(Fifo<u8>);

/// Represents a FIFO file (First In First Out), these files may be read from or written to.
///
/// Unlike other files which are implicitly opened when [Directory::get_file] is called, FIFO must
/// be opened explicitly by calling [Fifo::open]. The file must record the mode it is opened with and act appropriately.
///
/// A method must return [super::IoError::DeviceError] when
///
/// * An attempt to read or write to a FIFO is made before it is opened.
/// * If a read or write operation is made when the device is opened with the incorrect mode.
/// * If [Fifo::open] is called and the device cannot support more locks.
/// * [Fifo::close] is called without calling [Fifo::open]
///
/// A FIFO may support mastering, where a particular file object has more control over the device
/// than other file objects. Only one file object may be master for any file accessor at a time.
/// If no master is currently present when the FIFO is opened a new master may be chosen arbitrarily.
pub trait Fifo<T>: File + Read<T> + Write<T> {

    /// Locks this file accessor with the requested mode.
    fn open(&mut self, mode: OpenMode) -> Result<(), super::IoError>;

    fn close(&mut self) -> Result<(), super::IoError>;

    /// Returns the number of times that this file may be locked with this particular mode.
    ///
    /// Returning [usize::MAX] may indicate that this file can be locked an unlimited number of times.
    fn locks_remain(&self, mode: OpenMode) -> usize;

    /// Mastering is when one file object has higher permissions over others.
    ///
    /// If [Self::open] has been called returns whether this device supports mastering and whether
    /// this file object is currently the master.
    ///
    /// If [Self::open] has not been called this returns whether the file accessor has currently
    /// allocated a master.
    ///
    /// `None` indicates that mastering is not supported,
    /// `Some(true)` indicates that `self` is currently the master or that master has not yet been allocated.
    /// `Some(false)` indicates that `self` is not the master or that the master has been allocated
    fn is_master(&self) -> Option<usize>;
}

file_derive_debug!(FileSystem);
/// Trait for a base filesystem. A filesystem must implement this to be included in the VFS.
///
/// When the filesystem uses reference counting for referencing file accessors,
/// when [File::clone_file] is called on an implementor of this, it should use a strong reference and not a weak one.
/// This is because the VFS keeps `Box<dyn FileSystem>`'s to access the filesystem, using weak
/// references may cause the filesystem hierarchy to be dropped prematurely.
pub trait FileSystem: DeviceFile {
    /// Return the root directory of the filesystem.
    fn root(&self) -> alloc::boxed::Box<dyn Directory>;

    /// Returns the options the filesystem is configured with.
    /// These options may not change while the filesystem is mounted.
    ///
    /// See [FsOpts] for more info.
    fn get_opt(&self, option: &str) -> Option<FsOptionVariant>;

    /// Parse the `options` str for valid settings.
    /// `options` must be given as a whitespace seperated list of arguments.
    /// Unrecognized arguments may emmit a message to the user and be ignored,
    /// the filesystem is permitted to store unknown options.
    ///
    /// Any options which have been defined previously must be discarded when this function is called.
    ///
    /// This fn should only be called once when the filesystem is mounted to the VFS.
    /// If this fn is called multiple times the previous configuration should be discarded.
    ///
    /// ## Predefined options
    ///
    /// - NODEV: Disallows deice files
    /// - NOCACHE: Disables filesystem caching.
    ///
    /// Any extra options and their effects should documented in their implementation.
    fn set_opts(&mut self, options: &str);

    /// Return the name of the driver this filesystem is owned by.
    fn driver_name(&self) -> &'static str;

    /// If this filesystem is created from a file, return the file path.
    fn raw_file(&self) -> Option<&str>;

    /// This fn is called when the filesystem is unmounted from the VFS.
    /// Its purpose is to performs a clean-up before handing it off, so it may be used detached within the kernel.
    /// This fn will **not** be called when the filesystem is "remounted".
    /// Non-volatile filesystem drivers should not need to implement this, for clean-up prefer to use [Drop] instead.
    ///
    /// The default implementation of this method is a no-op nothing.
    fn unmount(&mut self) {}
}