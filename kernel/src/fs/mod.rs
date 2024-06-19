//! This module contains the "virtual file system" (VFS) and all components required to operate
//! and interface with it. It also contains
//!
//! This module also contains internal filesystem implementations

pub mod file;
pub mod device;
pub mod tmpfs;

type IoResult<'a,T> = futures_util::future::BoxFuture<'a, Result<T,IoError>>;

#[derive(Debug)]
pub enum IoError {
    /// Attempted to access a resource which is not present.
    ///
    /// This may also be returned when something *is* present when it is not expected.
    NotPresent,

    /// The media was unable to be read.
    ///
    /// This may be because the device has failed or is no longer attached to the system.
    MediaError,

    /// The device indicated that it encountered an error and could not be accessed.
    DeviceError,

    /// Returned when a file is requested to do something it does not support.
    /// Such as write to a read-only file
    NotSupported,

    /// This is returned when a file is locked for exclusive access
    /// or when a file is expected to be locked but isn't.
    Exclusive,

    /// This is returned when a file has no more data which can be read,
    /// or when a device has no more space than can be written to.
    EndOfFile,
}

/// Generic test for a FileSystem implementation.
///
/// This might panic on fail, it might throw an error.
#[cfg(test)]
async fn test_fs<F: file::Filesystem>(fs: F) -> crate::task::TaskResult {
    use crate::fs::file::*;

    fs.root().new_file("file", None).await.unwrap();
    let file = match fs.root().get_file("file").await.unwrap().unwrap().downcast() {
        FileTypeWrapper::Normal(file) => file,
        e => {
            log::warn!("Got incorrect file {:?}", e);
            return crate::task::TaskResult::Error
        }
    };
    let mut file = match NormalFile::file_lock( file ).await {
        Ok(f) => f,
        Err((e,_)) => panic!("Failed to clone `file` error: {:?}", e)
    };

    file.write(b"hello there").await.unwrap();

    let mut nf = downcast_file!(file.clone_file(), Normal).unwrap();

    assert_eq!(file.set_cursor(0).unwrap(),0,"Bad cursor move");
    let mut buff = alloc::vec::Vec::new();
    buff.resize(64,0);
    let read = file.read(&mut buff).await.unwrap();
    crate::println!("{}", core::str::from_utf8(read).unwrap());
    assert_eq!(read, b"hello there");
    nf.read(&mut buff).await.expect_err("Returned Ok");

    crate::task::TaskResult::ExitedNormally
}