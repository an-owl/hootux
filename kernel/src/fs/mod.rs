//! This module contains the "virtual file system" (VFS) and all components required to operate
//! and interface with it. It also contains
//!
//! This module also contains internal filesystem implementations

pub mod vfs;
pub mod file;
pub mod device;
pub mod tmpfs;
pub mod sysfs;

/// Contains the systems VFS. It may not be constructed until a root filesystem can be acquired.
///
/// Once the system is initialized this may never be `None`, and may be accessed using [Option::unwrap_unchecked].
///
/// See [vfs] for more info.
static mut VIRTUAL_FILE_SYSTEM: Option<alloc::boxed::Box<vfs::VirtualFileSystem>> = None;

pub const THIS_DIR: &str = ".";
pub const PARENT_DIR: &str = "..";
pub const PATH_SEPARATOR: char = '/';


/// Initializes the VFS,
pub fn init_fs(vfs: alloc::boxed::Box<dyn device::FileSystem>) {
    assert!(unsafe { VIRTUAL_FILE_SYSTEM.is_none() });
    log::debug!("Initializing VFS with: {} type: {}",vfs.device(), vfs.driver_name());
    unsafe { VIRTUAL_FILE_SYSTEM = Some(alloc::boxed::Box::new(vfs::VirtualFileSystem::new(vfs))); }
}

/// Returns a reference to the VFS.
///
/// # Safety
///
/// Technically this function should be unsafe. This is because is calls [Option::unwrap_unchecked]
/// however it is not possible for external code to call this fn before the VFS is initialized.
pub fn get_vfs() -> &'static vfs::VirtualFileSystem {
    let t =  unsafe { VIRTUAL_FILE_SYSTEM.as_ref() };
    unsafe { t.unwrap_unchecked() }
}

pub type IoResult<'a,T> = futures_util::future::BoxFuture<'a, Result<T,IoError>>;

#[derive(Debug)]
#[derive(PartialEq)]
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
    ///
    /// This may also be used to indicate that an invalid configuration was used for the device.
    /// This differs from [Self::MediaError] where this is returned when the device indicates an error.
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

    /// Returned when a file already exists with the name specified.
    AlreadyExists,

    /// Returned when attempting to delete a directory which is not empty.
    NotEmpty,

    /// Returned when attempting to modify a device file from a driver that cannot perform the operation.
    IsDevice,

    /// Attempted to modify a read only file.
    ReadOnly,

    /// Indicates that an action could not be completed because the file is in use by something else.
    Busy,

    /// Indicates that the file was not able to complete the operation because some prior
    /// requirements have not been met
    NotReady,

    /// Returned by [file::Write::write] when the input is an invalid variant of the character
    InvalidData
}

/// Generic test for a FileSystem implementation.
///
/// This might panic on fail, it might throw an error.
#[cfg(test)]
pub async fn test_fs<F: device::FileSystem + ?Sized>(fs: alloc::boxed::Box<F>) -> crate::task::TaskResult {
    use crate::fs::file::*;

    fs.root().new_file("file", None).await.unwrap();
    let file = cast_file!(NormalFile<u8>:fs.root().get_file("file").await.unwrap()).ok().expect("Expected NormalFile did not get one");

    let mut file = match NormalFile::file_lock( file ).await {
        Ok(f) => f,
        Err((e,_)) => panic!("Failed to clone `file` error: {:?}", e)
    };

    file.write(b"hello there").await.unwrap();

    let mut nf = cast_file!(NormalFile<u8>: file.clone_file()).ok().unwrap();

    assert_eq!(file.set_cursor(0).unwrap(),0,"Bad cursor move");
    let mut buff = alloc::vec::Vec::new();
    buff.resize(64,0);
    let read = file.read(&mut buff).await.unwrap();
    crate::println!("{}", core::str::from_utf8(read).unwrap());
    assert_eq!(read, b"hello there");
    nf.read(&mut buff).await.expect_err("Returned Ok");

    match fs.root().get_file(PARENT_DIR).await {
        Err(IoError::IsDevice) => {}
        _ => panic!("Expected IoError::IsDevice")
    }
    let parent_fh = fs.root().get_file_with_meta(PARENT_DIR).await.expect("Got Error when fetching `/..`").expect("`/..` does not exist");
    assert!(parent_fh.vfs_device_hint());

    crate::task::TaskResult::ExitedNormally
}