use crate::fs::file::*;
use crate::fs::sysfs::{SysfsDirectory, SysfsFile};
use crate::fs::{IoError, IoResult};
use acpi::AcpiTable;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;
use core::ops::Deref;
use futures_util::FutureExt;

const ACPI_INODE_ID: u64 = super::FIRMWARE_INODE_ID; // ACPI is offset 0

#[derive(Clone)]
#[file]
pub struct Tables {
    rsdt: alloc::sync::Arc<acpi::AcpiTables<crate::system::acpi::AcpiGrabber>>,
}

// SAFETY: AcpiTables is wrapped in a mutex and cannot be accessed by multiple threads.
unsafe impl Sync for Tables {}
unsafe impl Send for Tables {}

impl Tables {
    pub fn new(rsdt: acpi::AcpiTables<crate::system::acpi::AcpiGrabber>) -> Tables {
        Self {
            rsdt: alloc::sync::Arc::new(rsdt),
        }
    }

    pub fn get_table<T: acpi::AcpiTable>(&self) -> Result<SdtFile<T>, IoError> {
        Ok(SdtFile {
            table: alloc::sync::Arc::new(self.rsdt.find_table().map_err(|_| IoError::MediaError)?),
        })
    }
}

impl File for Tables {
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
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        ACPI_INODE_ID
    }

    fn len(&self) -> IoResult<u64> {
        async { Ok(SysfsDirectory::entries(self) as u64) }.boxed()
    }
}

impl SysfsDirectory for Tables {
    fn entries(&self) -> usize {
        self.rsdt.headers().count()
    }

    fn file_list(&self) -> Vec<String> {
        // return an empty vector because native-files shouldn't leak
        let v = Vec::new();
        /*
        use acpi::AcpiTable as _;
        macro_rules! maybe_append {
            ($name:ty) => {
                if self.rsdt.find_table::<$name>() {
                    v.push($name::SIGNATURE.to_string());
                }
            };
        }

        maybe_append!(acpi::madt::Madt);
        maybe_append!(acpi::bgrt::Bgrt);
        maybe_append!(acpi::fadt::Fadt);
        maybe_append!(acpi::hpet::HpetTable);
        maybe_append!(acpi::mcfg::Mcfg);
        maybe_append!(acpi::spcr::Spcr);

         */

        v
    }

    fn get_file(&self, name: &str) -> Result<Box<dyn SysfsFile>, IoError> {
        macro_rules! match_table {
            ($($sig:literal => $name:ty),*) => {
                match name {
                    $( $sig => {
                        Ok(alloc::boxed::Box::new(SdtFile {
                            table: alloc::sync::Arc::new(self.rsdt.find_table::<$name>().map_err(|_| IoError::MediaError)?),
                        }))
                    })*
                    _ => Err(IoError::NotPresent)
                }
            }
        }

        match_table!(
            "MADT" => acpi::madt::Madt,
            "BGRT" => acpi::bgrt::Bgrt,
            "FADT" => acpi::fadt::Fadt,
            "HPET" => acpi::hpet::HpetTable,
            "MCFG" => acpi::mcfg::Mcfg,
            "SPCR" => acpi::spcr::Spcr
        )
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

//#[file]
cast_trait_object::impl_dyn_cast!(for <T> SdtFile<T> as File where {T: 'static + AcpiTable} => File);
cast_trait_object::impl_dyn_cast!(for <T> SdtFile<T> as File where {T: 'static + AcpiTable} => NormalFile);
cast_trait_object::impl_dyn_cast!(for <T> SdtFile<T> as File where {T: 'static + AcpiTable} => Directory);
cast_trait_object::impl_dyn_cast!(for <T> SdtFile<T> as File where {T: 'static + AcpiTable} => crate::fs::device::FileSystem);
cast_trait_object::impl_dyn_cast!(for <T> SdtFile<T> as File where {T: 'static + AcpiTable} => crate::fs::device::Fifo<u8>);
cast_trait_object::impl_dyn_cast!(for <T> SdtFile<T> as File where {T: 'static + AcpiTable} => crate::fs::device::DeviceFile);

#[derive(Clone)]
pub struct SdtFile<T: acpi::AcpiTable + 'static> {
    table: alloc::sync::Arc<acpi::PhysicalMapping<crate::system::acpi::AcpiGrabber, T>>,
}

impl<T: acpi::AcpiTable + 'static> File for SdtFile<T> {
    fn file_type(&self) -> FileType {
        FileType::Native
    }

    fn block_size(&self) -> u64 {
        crate::mem::PAGE_SIZE as u64
    }

    fn device(&self) -> DevID {
        DevID::new(crate::fs::vfs::MajorNum::new_kernel(1), 0)
    }

    fn clone_file(&self) -> Box<dyn File> {
        // Box::new(self.clone()) returning Box<&self> for some reason?
        let t = Box::new(Self {
            table: self.table.clone(),
        });
        t
    }

    fn id(&self) -> u64 {
        // This uses the signature to form the higher half of the ID
        // ACPI defines the signature as 4 byte string. If this fails its probably because the signature contained a non-unicode character
        // we're forming a "hash" so the ordering doesn't really matter
        let bytes = u32::from_be_bytes(T::SIGNATURE.as_str().as_bytes().try_into().unwrap()) as u64;
        bytes << 32 | ACPI_INODE_ID
    }

    fn len(&self) -> IoResult<u64> {
        async { Ok(self.table.region_length() as u64) }.boxed()
    }
}

impl<T: acpi::AcpiTable + 'static> SysfsFile for SdtFile<T> {}

impl<T: acpi::AcpiTable + 'static> Deref for SdtFile<T> {
    type Target = acpi::PhysicalMapping<crate::system::acpi::AcpiGrabber, T>;
    fn deref(&self) -> &Self::Target {
        let t = &*self.table;
        t
    }
}

// SAFETY: We do not allow any mutation of the tables
unsafe impl<T: acpi::AcpiTable + 'static> Sync for SdtFile<T> {}
unsafe impl<T: acpi::AcpiTable + 'static> Send for SdtFile<T> {}

/// Convenience fn to fetch an ACPI table from the sysfs
pub fn get_table<T: acpi::AcpiTable>() -> Option<SdtFile<T>> {
    let fw = &crate::fs::sysfs::SysFsRoot::new().firmware;
    let acpi = super::super::SysfsDirectory::get_file(fw, "acpi").ok()?;
    let tables = acpi
        .into_sysfs_dir()
        .map(|fo| fo.as_any().downcast::<Tables>().unwrap())
        .unwrap();
    tables.get_table().ok()
}
