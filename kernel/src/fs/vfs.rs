/// This module contains th Virtual File System. The VFS is responsible for operating the system-wide
/// filesystem. This includes managing pseudo files, like pipes FIFOs and character devices.
///
/// The VFS is designed to handle the kernel-side of the libc filesystem interface.
/// This allows general purpose filesystem operations, the VFS does not handle file operations such
/// as reading/writing, these operations are handled directly by a driver.
///
/// As a quirk of how the VFS handles file traversal a path with a trailing `/` will always return a directory.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use core::fmt::{Display, Formatter};
use cast_trait_object::DynCastExt;
use futures_util::FutureExt as _;
use crate::fs::IoError;
use crate::fs::device::{DeviceFile, FileSystem};
use super::file::*;

pub type VfsFuture<'a, T> = futures_util::future::BoxFuture<'a, Result<T, VfsError>>;
type FileId = (DevID, u64);


pub struct VirtualFileSystem {
    root: Box<dyn FileSystem>,
    device_ctl: spin::RwLock<DeviceCtl>,
}

impl VirtualFileSystem {
    pub(crate) fn new(root: Box<dyn FileSystem>) -> Self {
        let this = Self {
            root,
            device_ctl: spin::RwLock::new(DeviceCtl{
                mounts: MountPoints{ mount_list: BTreeMap::new() },
                dev_override: DevOverrides::new()
            }),
        };

        let mut b = this.device_ctl.write();
        let _ = b.mounts.insert(MountDescription::new(this.root.clone_file().dyn_cast().unwrap(),"/".to_string(), String::new())).unwrap(); // This is the first entry so this will never return Err(_)
        drop(b);
        this
        // todo vfs-persistent pseudo filesystems should be mounted here
    }

    pub fn open<'a>(&'a self, path: &'a str) -> VfsFuture<Box<dyn File>> {
        async {
            path.is_absolute()?;
            let mut file = self.root.root().clone_file();
            for (i, filename) in path.split(super::PATH_SEPARATOR).enumerate() {
                // "" is treated as this file as is "." so we just skip them.
                // skipping "." could cause bugs... maybe?
                if filename.is_empty() || filename == super::THIS_DIR {
                    continue;
                }

                let dir = cast_dir!(file).map_err(|e: Box<dyn File>| {
                    // Returns the correct error
                    match e.file_type() {
                        #[cfg(not(debug_assertions))]
                        FileType::Directory => unreachable!(),
                        #[cfg(debug_assertions)]
                        FileType::Directory => panic!("Bad File imp.\nFailed to cast do Directory but file type is {:?}", FileType::Directory),
                        _ => VfsError::NotADirectory(i),
                    }
                })?;

                let file_handle = dir.get_file_with_meta(filename).await.map_err(|e|
                    if let IoError::NotPresent = e {
                        VfsError::DoesNotExist(i)
                    } else {
                        VfsError::LowerLevel(e)
                    })?;

                // This is only a hint and may not actually be a dev file
                if file_handle.vfs_device_hint() {
                    if let Some(id) = self.device_ctl.read().dev_override.lookup((dir.device(), dir.id()), filename) {
                        file = self.device_ctl.read().mounts.get_dev(id).expect("VFS could not find managed device file").file.clone_file();
                        continue // next file is found, now continue the loop
                    }
                }
                file = file_handle.file().expect("Filesystem did not return file.\nIt either hinted a device file and one was not found or did not return a file at all");
            }

            match cast_file!(FileSystem: file) {
                Ok(fs) => Ok(fs.dyn_upcast()),
                Err(file) => Ok(file)
            }
        }.boxed()
    }

    /// Attempts to traverse the filesystem to the directory containing the requested file.
    ///
    /// On success this will return the requested directory, the remaining path segment and the depth of the returned file.
    ///
    /// In the path `/usr/lib/share` a depth of `0` refers to `usr` a depth of `2` refers to `share`.
    fn traverse_to_dir<'a>(&'a self, path: &'a str) -> VfsFuture<(Box<dyn Directory>,&'a str , usize)> {
        async {
            //path.is_absolute()?;
            // p_iter[0] is "" because of leading slash
            let mut p_iter = path.split(super::PATH_SEPARATOR);

            let mut c_dir = self.root.root();

            let last = p_iter.next_back().unwrap(); // is_absolute guarantees that this is always some
            let mut depth = 0;
            let _ = p_iter.next(); // returns "" we ignore this. Using skip() makes this not double ended
            // Does not consume last element in p_iter
            // traverses to the directory containing the requested file
            for f_name in p_iter {
                depth += 1;
                if f_name == "." || f_name == "" {
                    continue
                }


                match self.traverse_file(&*c_dir,f_name).await {
                    Ok(f) => {
                        // Directory might be filesystem.
                        c_dir = match f.dyn_cast() {
                            Ok(f) => f,
                            Err(file) => cast_file!(FileSystem: file).map_err(|_| VfsError::NotADirectory(depth))?.root()
                        }
                    },
                    Err(VfsError::DoesNotExist(_)) => Err(VfsError::DoesNotExist(depth))?,
                    Err(e) => return Err(e), // this would be unreachable_unchecked() if `traverse_file` did not return Err(LowerLevel(_))
                }
            }

            Ok((c_dir, last ,depth))
        }.boxed()
    }

    /// Attempts to fetch `name` from `dir`, recalling the file from the managed-device list where necessary.
    ///
    /// # Errors
    ///
    /// Errors what return a path depth will contain [usize::MAX], the caller should set this to the appropriate value.
    fn traverse_file<'a>(&'a self, dir: &'a dyn Directory, name: &'a str) -> VfsFuture<Box<dyn File>> {
        async move {
            if name == "." || name == "" {
                return Ok(dir.clone_file())
            }

            match dir.get_file_with_meta(name).await {
                // file is a device
                Ok(FileHandle{file: None, device_hint: true, .. } ) => {
                    let id = self.device_ctl.read().dev_override.lookup((dir.device(),dir.id()),name).expect("Filesystem returned device hind with no file and the VFS found no corresponding device");
                    Ok(self.device_ctl.read().mounts.get_dev(id).expect("Dev override found device but found no such device").file.clone_file())
                }

                // file may be device
                Ok(FileHandle{file: Some(file),device_hint: true, .. } ) => {
                    // gibberish. Performs a lookup in self, if there is no suck device uses `file` instead
                    let dcl =  self.device_ctl.read();
                    let f = self.device_ctl.read().dev_override.lookup((dir.device(),dir.id()),name).and_then(|dev| dcl.mounts.get_dev(dev)).and_then(|d| Some(d.file.clone_file())).unwrap_or(file);
                    Ok(f)
                }

                // file is not device
                Ok(FileHandle{file: Some(file), device_hint: false, .. }) => {
                    // skip the file check here, it just adds extra ops
                    Ok(file)
                }
                Err(IoError::NotPresent) => Err(VfsError::DoesNotExist(usize::MAX))?,
                Err(e) => Err(VfsError::LowerLevel(e)),
                _ => unreachable!() // buggy drivers may reach this
            }
        }.boxed()
    }

    /// Creates a new directory at the specified path, returns a file objet to the new directory.
    pub fn mkdir<'a>(&'a self, path: &'a str) -> VfsFuture<Box<dyn Directory>> {
        async {
            path.is_absolute()?;
            let (dir, name, _) = self.traverse_to_dir(path).await?;
            let t = dir.new_dir(name).await.map_err( |e| VfsError::LowerLevel(e))?;
            Ok(t)
        }.boxed()
    }

    pub fn new_file<'a,>(&'a self, path: &'a str, file: Option<&'a mut dyn NormalFile<u8>>) -> VfsFuture<()> {
        async {
            path.is_absolute()?;
            let (dir, name, _) = self.traverse_to_dir(path).await?;
            // The second option is only returned when a source file is copied.
            // This only returns error when one of the inner options are some.
            let t = dir.new_file(name,file).await;
            t.map_err(|(e,_)| VfsError::LowerLevel(e.unwrap()))
        }.boxed()
    }

    /// Attempts to remove the file from the filesystem.
    ///
    /// If the file is a mountpoint this will fail with  `Err(IsDevice)`,
    /// The file must be removed via [Self::umount] instead.
    pub fn remove<'a>(&'a self, path: &'a str) -> VfsFuture<()> {
        async {
            path.is_absolute()?;
            let (file, name, _) = self.traverse_to_dir(path).await?;
            match file.remove(name).await {
                Err(IoError::IsDevice) => {
                    // this may fail silently, this variant is only a hint
                    let mut devs = self.device_ctl.write();
                    if let Some(id) = devs.dev_override.lookup((file.device(),file.id()), name) {
                        let file = devs.mounts.get_dev(id).unwrap().file.clone_file();
                        match cast_file!(&FileSystem: &*file) {
                            Ok(_) => {
                                log::warn!("Attempted to remove {name}: is a mount-point, use unmount() instead");
                                return Err(VfsError::LowerLevel(IoError::IsDevice))
                            }
                            Err(_) => {
                                // this will always succeed, this branch will not be taken if this isn't present
                                // We also lock device_ctl so order of operations is not affected.
                                let dev = devs.dev_override.remove((file.device(),file.id()),name).unwrap();
                                devs.mounts.remove(dev);
                            }
                        }
                    }
                    Ok(())
                }
                e=> e.map_err(|e| e.into()),
            }
        }.boxed()
    }

    /// Attempts to mount a new filesystem at the indicated mountpoint with the specified options.
    ///
    /// `vfs_options` are options given to the VFS for when the filesystem is mounted.
    /// `options` is passed to the filesystem driver to be parsed.
    ///
    /// # Errors
    ///
    /// - The requested path must allow mounting device files.
    /// - `fs.device()` must not return [DevID::NULL].
    /// - Excluding the last segment `mountpoint` must be a valid accessible location in the filesystem.
    // todo should this be sync?
    pub fn mount<'a>(&'a self, mut fs: Box<dyn FileSystem>, mountpoint: &'a str, _vfs_options: MountFlags, options: &'a str) -> VfsFuture<()> {
        async {

            //log::info!("Mounting {} to {mountpoint} ({})", fs.device(), fs.raw_file().unwrap_or(fs.driver_name()) );

            fs.set_opts(options);
            self.mount_dev( cast_file!(DeviceFile: fs.dyn_upcast()).ok().unwrap(),mountpoint).await

        }.boxed()
    }

    /// Mounts a device file into the filesystem at `mountpoint`.
    /// This requires that the target mountpoint allows device files to be mounted.
    /// This function must not be called when `dev` us a [FileSystem].
    ///
    /// # Errors
    ///
    /// - The requested path must allow mounting device files.
    /// - `fs.device()` must not return [DevID::NULL].
    /// - Excluding the last segment `mountpoint` must be a valid accessible location in the filesystem.
    pub fn mount_dev<'a>(&'a self, dev: Box<dyn DeviceFile>, mountpoint: &'a str) -> VfsFuture<()> {
        async move {
            mountpoint.is_absolute()?;
            let id = dev.device();
            if id == DevID::NULL {
                log::error!("Attempted to attach device with DevID::NULL; Not allowed.");
                return Err(VfsError::InvalidArg);
            }

            // gets index of last path-sep and splits with index
            // absolute path must contain one path-sep this will never return None
            // +1 causes the path-sep to be in `path` not `f_name`
            let (path, f_name) = mountpoint.split_at(mountpoint.rfind(super::PATH_SEPARATOR).unwrap() + 1);
            log::debug!("Attempting to mount {} to {path}@{f_name}", id);

            log::debug!("{path}");
            // Acquire filesystem for `mountpoint`
            let dir = cast_file!(Directory: self.open(path).await?).map_err(|_| {
                log::error!("Attempted to mount device file to non directory file");
                VfsError::InvalidArg
            })?;
            let mut l = self.device_ctl.write();
            let fs = l.mounts.get_dev(dir.device()).unwrap(); // None is a bug

            // ensure that filesystem allows mounting device files
            let fs = if let Ok(fs) = cast_file!(FileSystem: fs.file.clone_file().dyn_upcast()) {
                if fs.get_opt(FsOpts::DEV_ALLOWED).unwrap() == FsOpts::FALSE { // const args are always present
                    log::error!("While attempting to mount {} filesystem at {path} {} does not allow mounting device files. Operation was aborted", id, fs.device()); // returning Err is a kernel panic waiting to happen anyway
                    return Err(VfsError::InvalidArg);
                };
                fs
            } else {
                panic!("Attempted to mount device that is not filesystem");
            };

            // fs may require looking up dev in VFS managed table, so we store it here and remove if an error occurs
            let new_description = MountDescription::new(dev.clone_file().dyn_cast().ok().unwrap(),mountpoint.to_string(), String::new());

            if let Err(_) = l.mounts.insert(new_description) {
                log::error!("Device already mounted {id}");
                return Err(VfsError::LowerLevel(IoError::AlreadyExists))
            }

            let dev_id=  dev.device();
            match dir.store(f_name, dev.dyn_upcast()).await {

                // success using native mount
                Ok(()) => {
                    log::info!("Mounted {} at {mountpoint} ({}) using native mount",id, fs.device());
                    Ok(())
                },

                // Indicates that FS does not support native dev lookups and VFS lookups must be used.
                Err(IoError::NotSupported) => {
                    l.dev_override.store((dir.device(), dir.id()), f_name.to_string(), dev_id);
                    log::info!("Mounted {} at {mountpoint} ({}) using VFS lookup", id, fs.device());
                    return Ok(())
                }

                // actual error
                Err(e) => {
                    l.mounts.remove(dev_id);
                    log::error!("While attempting to mount {} to {mountpoint}: {e:?}", id);
                    return Err(VfsError::LowerLevel(e))
                }
            }
        }.boxed()
    }

    pub fn umount(&self, dev: &str) -> Option<Box<dyn DeviceFile>> {
        self.device_ctl.read().mounts.search(dev).map(|d| d.file.clone_file().dyn_cast().ok().unwrap())
    }

    pub fn file_list<'a>(&'a self, path: &'a str) -> VfsFuture<alloc::vec::Vec<String>> {
        async {
            path.is_absolute()?;
            let (dir,name,depth) = self.traverse_to_dir(path).await?;
            let new_dir = cast_file!(Directory: self.traverse_file(&*dir,name).await?).map_err(|_| VfsError::NotADirectory(depth + 1))?;

            // fixme requires 2 vec allocations and a realloc.
            // that is too much.
            let mut list = new_dir.file_list().await.map_err(|e| VfsError::LowerLevel(e))?;
            if let Some(mut l) = self.device_ctl.read().dev_override.in_dir((new_dir.device(),new_dir.id())) {
                list.append(&mut l)
            }

            Ok(list)

        }.boxed()
    }
}

/// DeviceCtl handles device file bindings.
struct DeviceCtl {
    /// Contains data and metadata about a device file.
    mounts: MountPoints,

    /// Used to perform lookups to locate device files that exist with the filesystem.
    dev_override: DevOverrides
}

bitflags::bitflags! {
    // 32bits this struct may need to be passed via a syscall.
    // on 32bit systems a larger struct may be troublesome
    pub struct MountFlags: u32 {
        const READ_ONLY = 1;
        const NO_EXECUTE = 1 << 1;
    }
}

trait FilePath {

    /// Returns whether `self` is an absolute path. Returns `Err(VfsError::PathFramingError)` if it's not.
    ///
    /// The return type is intended to make handling this easier, the caller should be able to use
    /// the `?` operator to handle an error.
    fn is_absolute(&self) -> Result<(),VfsError>;

    /// See [Self::is_absolute] but for relative paths instead.
    #[inline]
    #[allow(dead_code)]
    fn is_relative(&self) -> Result<(),VfsError> {
        if self.is_absolute().is_ok() {
            Err(VfsError::PathFrameError)
        } else {
            Ok(())
        }
    }
}

impl FilePath for str {

    #[inline]
    fn is_absolute(&self) -> Result<(), VfsError> {
        if self.starts_with("/") {
            Ok(())
        } else {
            Err(VfsError::PathFrameError)
        }
    }
}

#[derive(Debug)]
pub enum VfsError {
    NotADirectory(usize),
    DoesNotExist(usize),
    InvalidArg,
    LowerLevel(IoError),
    /// Returned when the presence or lack of a leading `/` was unexpected
    PathFrameError,
}

impl From<IoError> for VfsError {
    fn from(value: IoError) -> Self {
        Self::LowerLevel(value)
    }
}

/// This is used to uniquely identify devices.
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
#[repr(C)]
pub struct DevID(usize, usize);

impl DevID {
    /// Attempting to locate this device from the kernel will always return `None`
    /// This may be used when a driver does not want a device file to appear globally.
    pub const NULL: Self = Self(0,0);

    // todo add make major a private type
    pub fn new(maj: MajorNum, minor: usize) -> Self {
        Self(maj.0,minor)
    }
}

/// Major number must be acquired from the kernel.
/// Minor numbers can be allocated by the driver owning the major number.
///
/// A series of numbers are reserved and may not be allocated at runtime and are reserved for kernel use.
/// Each reserved value and its use is documented here.
///
/// <ol start=0>
///     <li> Invalid: may not be used </li>
///     <li> sysfs: All sysfs components use this major number </li>
/// </ol>
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub struct MajorNum(usize);

impl MajorNum {

    /// Lowest value for publicly available Major numbers.
    /// All values below this must be documented in the [Self]'s doc comment.
    const PUBLIC_BASE: usize = 2;

    pub fn new() -> Self {
        static NEXT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(MajorNum::PUBLIC_BASE); // 0 is not allowed.
        Self(NEXT.fetch_add(1,atomic::Ordering::Relaxed))
    }

    /// Private constructor for reserved major numbers.
    ///
    /// Callers must ensure that `n` is less than [Self::PUBLIC_BASE]
    pub(crate) const fn new_kernel(n: usize) -> Self {
        debug_assert!(n < Self::PUBLIC_BASE);
        Self(n)
    }
}

impl Display for DevID {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        core::write!(f, "{}:{}",self.0,self.1)
    }
}

// todo add link count, so mountpoints may be used multiple times
struct MountPoints {
    // todo convert this to BtreeMap<DevID,Vec<_>>, this allows multiple mount points for a single device.
    mount_list: BTreeMap<DevID,MountDescription>,
}

impl MountPoints {
    fn search(&self, needle: &str) -> Option<&MountDescription> {
        for (_,element) in self.mount_list.iter() {
            if element.location == needle {
                return Some(element)
            } else if element.dev.as_ref().is_some_and(|i| i == needle ) {
                return Some(element)
            }
        }
        None
    }

    fn get_dev(&self, id: DevID) -> Option<&MountDescription> {
        self.mount_list.get(&id)
    }

    fn insert(&mut self, desc: MountDescription) -> Result<(),MountDescription> {
        match self.mount_list.insert(desc.file.device(),desc) {
            Some(d) => Err(d),
            None => Ok(())
        }
    }

    fn remove(&mut self, id: DevID) -> Option<MountDescription> {
        self.mount_list.remove(&id)
    }
}

#[allow(dead_code)] // some info here is not required yet.
#[derive(Debug)]
struct MountDescription {
    file: Box<dyn DeviceFile>,
    is_fs: bool,
    dev: Option<String>,
    location: String,
    ty: Option<String>,
    options: String,
}

impl MountDescription {
    fn new(dev: Box<dyn DeviceFile>, mountpoint: String, options: String, ) -> Self {
        let fs = cast_file!(&FileSystem: (&*dev).dyn_upcast());

        let name= if let Ok(f) = fs {
            f.raw_file().map(|s| s.to_string())
        } else {
            None
        };

        Self {
            is_fs: fs.is_ok(),
            dev: name,
            location: mountpoint,
            ty: fs.map(|fs| fs.driver_name().to_string()).ok(),
            options,
            file: dev,
        }
    }
}

struct DevOverrides {
    map: BTreeMap<FileId, BTreeMap<String, DevID>>
}

impl DevOverrides {
    const fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }

    fn in_dir(&self, id: FileId ) ->  Option<alloc::vec::Vec<String>> {
        Some(self.map.get(&id)?.keys().map(|s| s.clone()).collect())
    }

    fn lookup(&self, id: FileId, file: &str) -> Option<DevID> {
        Some(self.map.get(&id)?.get(file)?.clone())
    }

    fn store(&mut self, file_id: FileId, name: String, device: DevID) {
        self.map.entry(file_id).or_insert(BTreeMap::new()).insert(name,device);
    }

    fn remove(&mut self, file_id: FileId, name: &str) -> Option<DevID> {
        let dir = self.map.get_mut(&file_id)?;
        let file = dir.remove(name)?;
        if dir.is_empty() {
            let _ = self.map.remove(&file_id);
        }
        Some(file)
    }
}

/*
async fn test_vfs() -> task::TaskResult {
    use crate::fs::file::*;
    let vfs = fs::get_vfs();
    vfs.new_file("/new", None).await.unwrap();
    let mut new = cast_file!(NormalFile<u8>: vfs.open("/new").await.unwrap()).ok().unwrap();
    new.write(b"Hello there").await.unwrap();

    log::debug!("cursor: {}", new.move_cursor(0).unwrap());
    new.set_cursor(0).unwrap();
    let mut buff = Vec::new();
    buff.resize(11,0);
    log::debug!("{}", core::str::from_utf8(new.read(&mut *buff).await.unwrap()).unwrap());

    let f_list = vfs.file_list("/").await.unwrap();
    assert_eq!(f_list.len(), 1);
    assert_eq!(f_list[0],"new");

    let mount_leaf = fs::tmpfs::TmpFsRoot::new();
    vfs.mount( cast_file!(fs::device::FileSystem: mount_leaf.clone_file()).ok().unwrap(), "/mnt", fs::vfs::MountFlags::empty(), "").await.unwrap();

    let mut flags = 0;
    let ls = vfs.file_list("/").await.unwrap();
    assert_eq!(ls.len(), 2);
    for i in &ls {
        if i == "new" {
            flags |= 1;
        } else if i == "mnt" {
            flags |= 2;
        }
    }
    assert_eq!(flags, 3);

    for i in &ls {
        print!("{i} ");
    }
    println!();

    vfs.mkdir("/mnt/dir").await.unwrap();
    vfs.new_file("/mnt/dir/file",None).await.unwrap();
    let mut file = cast_file!(NormalFile<u8>: vfs.open("/mnt/dir/file").await.unwrap()).ok().unwrap();

    file.write(b"Hello, world!").await.unwrap();
    assert_eq!(file.device(), mount_leaf.device());
    assert_ne!(file.device(), new.device());
    log::debug!("{}",mount_leaf.device());

    let root = mount_leaf.root();
    let dir = cast_file!(Directory: root.get_file("dir").await.unwrap().unwrap()).ok().unwrap();
    println!("dir file list");
    for i in dir.file_list().await.unwrap() {
        print!("{i} ");
    }
    println!();
    let file_t = dir.get_file("file").await.unwrap().unwrap();
    let mut vec = Vec::new();
    vec.resize(32,0);
    let mut file_t = cast_file!(NormalFile<u8>:file_t).unwrap();
    let b= file_t.read(&mut vec).await.unwrap();

    log::debug!("{}",core::str::from_utf8(b).unwrap());
    assert_eq!(file_t.id(),file.id());

    task::TaskResult::ExitedNormally
}
 */