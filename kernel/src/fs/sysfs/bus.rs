use super::super::IoResult;
use super::{SysfsDirectory, SysfsFile};
use crate::fs::IoError;
use crate::fs::file::*;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::Any;
use futures_util::FutureExt;
use hootux::fs::vfs::MajorNum;

trait IndexExtension {
    type FileType: ?Sized;

    fn len(&self) -> usize;
    fn store(&self, name: &str, file: Box<Self::FileType>) -> Result<(), IoError>;
    fn remove(&self, name: &str) -> Result<(), IoError>;
}

impl IndexExtension
    for alloc::sync::Arc<spin::RwLock<alloc::collections::BTreeMap<&'static str, UniqueBus>>>
{
    type FileType = dyn SysfsFile;

    fn len(&self) -> usize {
        self.read().len()
    }

    fn store(&self, _name: &str, _file: Box<Self::FileType>) -> Result<(), IoError> {
        Err(IoError::NotSupported)
    }

    fn remove(&self, _name: &str) -> Result<(), IoError> {
        Err(IoError::NotSupported)
    }
}

/* todo:
 * adopt Linux model sys/bus/busname/{devices|drivers}
 * add event file for each bus. Returns bus IDs for each device event. Initially will iterate over existing IDs to simplify device seeking.
 */

/// Sysfs-Bus File-object.
#[derive(Clone)]
#[kernel_proc_macro::file]
pub struct SysfsBus {
    event: super::EventFile,
    files: alloc::sync::Arc<spin::RwLock<alloc::collections::BTreeMap<&'static str, UniqueBus>>>,
}

impl SysfsBus {
    const STATIC_FILES_NAMES: &'static [&'static str] = &["event"];
}

impl SysfsDirectory for SysfsBus {
    fn entries(&self) -> usize {
        self.files.read().keys().len() + Self::STATIC_FILES_NAMES.len()
    }

    fn file_list(&self) -> Vec<String> {
        self.files
            .read()
            .keys()
            .map(|s| s.to_string())
            .chain(Self::STATIC_FILES_NAMES.into_iter().map(|s| s.to_string()))
            .collect()
    }

    fn get_file(&self, name: &str) -> Result<Box<dyn SysfsFile>, IoError> {
        match name {
            "event" => Ok(Box::new(self.event.clone())),
            _ => self
                .files
                .read()
                .get(name)
                .map(|d| Box::new(d.clone()) as Box<dyn SysfsFile>)
                .ok_or(IoError::NotSupported),
        }
    }

    fn store(&self, _: &str, _: Box<dyn SysfsFile>) -> Result<(), IoError> {
        log::warn!(
            "called <{0} as SysfsDirectory>::store() use {0}::insert_device() instead",
            core::any::type_name::<Self>()
        );
        Err(IoError::NotSupported)
    }

    fn remove(&self, _: &str) -> Result<(), IoError> {
        Err(IoError::NotSupported)
    }

    fn as_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

impl File for SysfsBus {
    fn file_type(&self) -> FileType {
        FileType::Directory
    }

    fn block_size(&self) -> u64 {
        crate::mem::PAGE_SIZE as u64
    }

    fn device(&self) -> DevID {
        DevID::new(MajorNum::new_kernel(1), 0)
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        1
    }

    fn len(&self) -> IoResult<u64> {
        async { Ok(self.files.read().len() as u64) }.boxed()
    }
}

impl SysfsBus {
    pub(super) fn init() -> Self {
        Self {
            event: super::EventFile::new(),
            files: alloc::sync::Arc::new(spin::RwLock::new(alloc::collections::BTreeMap::new())),
        }
    }

    pub fn new_bus(&self, name: &'static str) -> Result<(), ()> {
        let Some(_) = self.files.write().insert(name, UniqueBus::new(name)) else {
            return Ok(());
        };
        Err(())
    }

    pub fn insert_device(&self, device: Box<dyn BusDeviceFile>) {
        let s = BusDeviceFile::id(&*device);

        let bus_name = device.bus();

        let mut rl = self.files.read();
        let bus = if let Some(bus) = rl.get(bus_name) {
            bus
        } else {
            // insert new bus when it's not present
            drop(rl);
            self.new_bus(bus_name)
                .expect("failed to insert bus, but could not locate bus");
            rl = self.files.read();
            rl.get(bus_name).unwrap() // Always returns Some()
        };

        SysfsDirectory::store(bus, &*s, super::clone_sysfs_file(&*device)).unwrap();
        self.event.notify_event()
    }
}

/// Contains a single bus type e.g. USB, PCI.
///
/// Bus names must be lowercase, use latin characters, and should be concise.
#[kernel_proc_macro::file]
#[derive(kernel_proc_macro::SysfsDir, Clone)]
#[allow(dead_code)]
struct UniqueBus {
    name: &'static str, // todo check that devices actually belong to this bus.
    #[index(getter=self.devices.read().get(name).ok_or(IoError::NotPresent).map(|f| super::clone_sysfs_file(&**f)),keys=self.devices.read().keys().map(|s| s.clone()))]
    devices: alloc::sync::Arc<
        spin::RwLock<alloc::collections::BTreeMap<alloc::string::String, Box<dyn SysfsFile>>>,
    >,
    serial: u64,
}

impl File for UniqueBus {
    fn file_type(&self) -> FileType {
        FileType::Directory
    }
    fn block_size(&self) -> u64 {
        crate::mem::PAGE_SIZE as u64
    }
    fn device(&self) -> DevID {
        DevID::new(MajorNum::new_kernel(1), 0)
    }
    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }
    fn id(&self) -> u64 {
        self.serial
    }
    fn len(&self) -> IoResult<u64> {
        async { Ok(self.devices.len() as u64) }.boxed()
    }
}

impl IndexExtension
    for alloc::sync::Arc<
        spin::RwLock<alloc::collections::BTreeMap<alloc::string::String, Box<dyn SysfsFile>>>,
    >
{
    type FileType = dyn SysfsFile;
    fn len(&self) -> usize {
        self.read().len()
    }
    fn store(&self, name: &str, file: Box<Self::FileType>) -> Result<(), IoError> {
        let mut wl = self.write();
        let r = wl.insert(name.to_string(), super::clone_sysfs_file(&*file));
        if let Some(_) = r {
            Err(IoError::AlreadyExists)
        } else {
            Ok(())
        }
    }

    fn remove(&self, name: &str) -> Result<(), IoError> {
        let mut wl = self.write();
        wl.remove(name).ok_or(IoError::NotPresent)?;
        Ok(())
    }
}

impl UniqueBus {
    fn new(name: &'static str) -> Self {
        // allocates inode numbers.
        static BUS_SERIAL: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
        Self {
            name,
            devices: alloc::sync::Arc::new(spin::RwLock::new(alloc::collections::BTreeMap::new())),
            serial: BUS_SERIAL.fetch_add(1, atomic::Ordering::Relaxed),
        }
    }
}

/// A bus device file is a [Directory] owned by the bus driver.
/// It may contain any file the driver wishes to export into userspace.
/// Subordinate drivers (e.g. a device driver or vendor specific driver) may add any files as long
/// as they do not already exist.
///
/// A bus driver may specify a layout for any subordinate drivers, subordinate drivers are required to comply.
///
/// A bus device file may export files into the kernel by including the [crate::fs::PATH_SEPARATOR]
/// character in the filename. If a driver does this my must ensure that these filenames are not
/// returned by [Directory::file_type] or counted by [Directory::entries].
pub trait BusDeviceFile: SysfsFile {
    /// Returns the name of the bus.
    fn bus(&self) -> &'static str;

    /// Returns the bus ID of the device represented.
    fn id(&self) -> alloc::string::String;

    // This is required because sysfs wants to allow using concrete types wherever possible.
    fn as_any(self: Box<Self>) -> Box<dyn core::any::Any>;
}
