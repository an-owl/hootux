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
trait DeviceFile<T>: NormalFile<T> {}

enum OpenMode {
    Read,
    Write,
    ReadWrite,
}

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
trait Fifo<T>: File + Read<T> + Write<T> {

    /// Locks this file accessor with the requested mode.
    fn open(&mut self, mode: OpenMode) -> Result<(), super::IoError>;

    fn close(&mut self) -> Result<(), super::IoError>;

    /// Returns the number of times that this file may be locked with this particular mode.
    ///
    /// Returning [usize::MAX] may indicate that this file can be locked an unlimited number of times.
    fn locks_remain(&self, mode: OpenMode) -> usize;

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

impl<T> Seek for T where T: Fifo<_> {
    fn move_cursor(&mut self, _seek: i64) -> Result<u64, super::IoError> {
        Err (super::IoError::NotSupported)
    }

    fn set_cursor(&mut self, _pos: u64) -> Result<u64, super::IoError> {
        Err (super::IoError::NotSupported)
    }

    fn rewind(&mut self, _pos: u64) -> Result<u64, super::IoError> {
        Err (super::IoError::NotSupported)
    }

    fn seek(&mut self, _pos: u64) -> Result<u64, super::IoError> {
        Err (super::IoError::NotSupported)
    }
}