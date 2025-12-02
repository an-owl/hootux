use crate::fs::sysfs::{SysfsDirectory, SysfsFile, clone_sysfs_file};
use crate::fs::vfs::MajorNum;
use crate::fs::{IoError, IoResult};
use crate::mem::dma::DmaBuff;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use hootux::fs::file::*;

static PCI_MAJOR_NUM: spin::Mutex<Option<MajorNum>> = spin::Mutex::new(None);

fn get_major() -> MajorNum {
    let mut l = PCI_MAJOR_NUM.lock();
    if let Some(major) = l.as_mut() {
        *major
    } else {
        let n = MajorNum::new();
        *l = Some(n);
        n
    }
}

struct FunctionAccessor {
    addr: super::DeviceAddress,
    ctl: alloc::sync::Arc<async_lock::Mutex<super::DeviceControl>>,
    bound: hootux::fs::sysfs::BindingFile,
}

impl FunctionAccessor {
    fn dev_id(&self) -> DevID {
        DevID::new(get_major(), self.addr.as_int_joined() as usize)
    }
}

#[file]
#[derive(Clone)]
pub(super) struct FuncDir {
    accessor: alloc::sync::Arc<FunctionAccessor>,
}

impl FuncDir {
    pub(super) fn new(ctl: super::DeviceControl) -> Self {
        Self {
            accessor: alloc::sync::Arc::new(FunctionAccessor {
                addr: ctl.address(),
                ctl: alloc::sync::Arc::new(async_lock::Mutex::new(ctl)),
                bound: hootux::fs::sysfs::BindingFile::new(),
            }),
        }
    }
}

impl File for FuncDir {
    fn file_type(&self) -> FileType {
        FileType::Directory
    }

    fn block_size(&self) -> u64 {
        256
    }

    fn device(&self) -> DevID {
        self.accessor.dev_id()
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        self.accessor.addr.as_int_joined()
    }

    fn len(&self) -> IoResult<'_, u64> {
        async { Ok(SysfsDirectory::entries(self) as u64) }.boxed()
    }
}

impl SysfsDirectory for FuncDir {
    fn entries(&self) -> usize {
        2
    }

    fn file_list(&self) -> Vec<String> {
        vec!["class".to_string(), "cfg".to_string(), "bind".to_string()]
    }

    fn get_file(&self, name: &str) -> Result<Box<dyn SysfsFile>, IoError> {
        match name {
            "." => Ok(Box::new(self.clone())),
            ".." => Ok(
                SysfsDirectory::get_file(&hootux::fs::sysfs::SysFsRoot::new().bus, "pci")
                    .ok()
                    .unwrap(),
            ), // "pci" must exist if we've got here

            "class" => Ok(Box::new(Class {
                accessor: self.accessor.clone(),
            })),
            "cfg" => Ok(Box::new(ConfigRegionFile {
                accessor: self.accessor.clone(),
            })),
            "bind" => Ok(clone_sysfs_file(&self.accessor.bound)),
            _ => Err(IoError::NotPresent),
        }
    }

    fn store(&self, _: &str, _: Box<dyn SysfsFile>) -> Result<(), IoError> {
        Err(IoError::NotSupported)
    }

    fn remove(&self, _: &str) -> Result<(), IoError> {
        Err(IoError::NotSupported)
    }

    fn as_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

impl hootux::fs::sysfs::bus::BusDeviceFile for FuncDir {
    fn bus(&self) -> &'static str {
        "pci"
    }

    fn id(&self) -> String {
        self.accessor.addr.to_string()
    }

    fn as_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[file]
#[derive(Clone)]
struct Class {
    accessor: alloc::sync::Arc<FunctionAccessor>,
}

impl File for Class {
    fn file_type(&self) -> FileType {
        FileType::NormalFile
    }

    fn block_size(&self) -> u64 {
        4
    }

    fn device(&self) -> DevID {
        self.accessor.dev_id()
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        1
    }

    fn len(&self) -> IoResult<'_, u64> {
        async { Ok(3) }.boxed()
    }
}

impl NormalFile for Class {
    fn len_chars(&self) -> IoResult<'_, u64> {
        File::len(self)
    }

    fn file_lock<'a>(
        self: Box<Self>,
    ) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
        async { Err((IoError::NotSupported, self as _)) }.boxed()
    }

    unsafe fn unlock_unsafe(&self) -> IoResult<'_, ()> {
        async { Err(IoError::NotSupported) }.boxed()
    }
}

impl SysfsFile for Class {}

impl Read<u8> for Class {
    fn read(
        &self,
        _: u64,
        mut buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async {
            let blen = buff.len().max(3);
            let tgt = &mut buff[0..blen];
            let len = tgt.len();
            tgt.copy_from_slice(&self.accessor.ctl.lock().await.class[..len]);

            Ok((buff, len))
        }
        .boxed()
    }
}

impl Write<u8> for Class {
    fn write(
        &self,
        _: u64,
        buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async { Err((IoError::ReadOnly, buff, 0)) }.boxed()
    }
}

#[file]
#[derive(Clone)]
struct ConfigRegionFile {
    accessor: alloc::sync::Arc<FunctionAccessor>,
}

impl ConfigRegionFile {
    fn ctl_raw(&self) -> alloc::sync::Arc<async_lock::Mutex<super::DeviceControl>> {
        self.accessor.ctl.clone()
    }
}

impl File for ConfigRegionFile {
    fn file_type(&self) -> FileType {
        FileType::NormalFile
    }

    fn block_size(&self) -> u64 {
        4096
    }

    fn device(&self) -> DevID {
        self.accessor.dev_id()
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        2
    }

    fn len(&self) -> IoResult<'_, u64> {
        async { Ok(4096) }.boxed()
    }

    fn method_call<'f, 'a: 'f, 'b: 'f>(
        &'b self,
        method: &str,
        arguments: &'a (dyn Any + Send + Sync + 'static),
    ) -> IoResult<'f, MethodRc> {
        impl_method_call!(method, arguments => async ctl_raw())
    }
}

impl NormalFile for ConfigRegionFile {
    fn len_chars(&self) -> IoResult<'_, u64> {
        File::len(self)
    }

    fn file_lock<'a>(
        self: Box<Self>,
    ) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
        async { Err((IoError::NotSupported, self as _)) }.boxed()
    }

    unsafe fn unlock_unsafe(&self) -> IoResult<'_, ()> {
        async { Err(IoError::Exclusive) }.boxed()
    }
}

impl SysfsFile for ConfigRegionFile {}

impl Read<u8> for ConfigRegionFile {
    fn read(
        &self,
        pos: u64,
        mut buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async move {
            let Ok(pos) = pos.try_into() else {
                return Err((IoError::EndOfFile, buff, 0));
            };
            let cfg_region = &self.accessor.ctl.lock().await.cfg_region[pos..];
            let len = buff.len().max(cfg_region.len());
            buff[..len].copy_from_slice(&cfg_region[..len]);
            Ok((buff, len))
        }
        .boxed()
    }
}

impl Write<u8> for ConfigRegionFile {
    fn write(
        &self,
        _: u64,
        buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async {
            log::debug!(
                "Called write on {}, which currently does not allow writing",
                core::any::type_name::<Self>()
            );
            Err((IoError::ReadOnly, buff, 0))
        }
        .boxed()
    }
}
