use crate::fs::file::*;
use crate::fs::sysfs::SysfsFile;
use crate::fs::{IoError, IoResult};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;
use futures_util::FutureExt;

pub mod acpi;

const FIRMWARE_INODE_ID: u64 = 2;

#[file]
pub struct FirmwareContainer {
    acpi: alloc::sync::Arc<spin::Mutex<Option<acpi::Tables>>>, // ACPI may not be present and should be initialized separately
}

impl FirmwareContainer {
    pub fn new() -> FirmwareContainer {
        Self {
            acpi: alloc::sync::Arc::new(spin::Mutex::new(None)),
        }
    }

    /// Sets up the `./acpi` directory
    pub fn load_acpi(&self, rsdt: ::acpi::AcpiTables<crate::system::acpi::AcpiGrabber>) {
        let t = acpi::Tables::new(rsdt);
        core::mem::replace(&mut *self.acpi.lock(), Some(t))
            .ok_or(())
            .map(|_| ())
            .expect_err("sysfs-acpi module already initialized");
        assert!(self.acpi.lock().is_some())
    }
}

impl File for FirmwareContainer {
    fn file_type(&self) -> FileType {
        FileType::Directory
    }

    fn block_size(&self) -> u64 {
        crate::mem::PAGE_SIZE as u64
    }

    fn device(&self) -> DevID {
        DevID::new(crate::fs::vfs::MajorNum::new_kernel(1), 0)
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(Clone::clone(self))
    }

    fn id(&self) -> u64 {
        2
    }

    fn len(&self) -> IoResult<u64> {
        async { Ok(super::SysfsDirectory::entries(self) as u64) }.boxed()
    }
}
impl super::SysfsDirectory for FirmwareContainer {
    fn entries(&self) -> usize {
        let mut n = 0;
        n += self.acpi.lock().is_some() as usize;
        n
    }

    fn file_list(&self) -> Vec<String> {
        let mut vec = Vec::new();
        if self.acpi.lock().is_some() {
            vec.push(String::from("/"));
        }

        vec
    }

    fn get_file(&self, name: &str) -> Result<Box<dyn SysfsFile>, IoError> {
        match name {
            "acpi" => Ok(Box::new(
                self.acpi
                    .lock()
                    .as_ref()
                    .ok_or(IoError::NotPresent)?
                    .clone(),
            )),
            _ => Err(IoError::NotPresent),
        }
    }

    fn store(&self, _name: &str, _file: Box<dyn SysfsFile>) -> Result<(), IoError> {
        Err(IoError::NotSupported)
    }

    fn remove(&self, _name: &str) -> Result<(), IoError> {
        Err(IoError::NotSupported)
    }

    fn as_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

impl Clone for FirmwareContainer {
    fn clone(&self) -> Self {
        // The mutex here is exclusively here for the Option and not the Table.
        // So we can freely clone the mutex
        Self {
            acpi: self.acpi.clone(),
        }
    }
}
