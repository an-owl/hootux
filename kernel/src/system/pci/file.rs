use crate::fs::sysfs::{SysfsDirectory, SysfsFile};
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

    fn len(&self) -> IoResult<u64> {
        async { Ok(SysfsDirectory::entries(self) as u64) }.boxed()
    }
}

impl SysfsDirectory for FuncDir {
    fn entries(&self) -> usize {
        2
    }

    fn file_list(&self) -> Vec<String> {
        vec!["class".to_string(), "cfg".to_string()]
    }

    fn get_file(&self, name: &str) -> Result<Box<dyn SysfsFile>, IoError> {
        match name {
            "class" => Ok(Box::new(Class {
                accessor: self.accessor.clone(),
            })),
            "cfg" => Ok(Box::new(ConfigRegionFile {
                accessor: self.accessor.clone(),
            })),
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

    fn len(&self) -> IoResult<u64> {
        async { Ok(3) }.boxed()
    }
}

impl NormalFile for Class {
    fn len_chars(&self) -> IoResult<u64> {
        File::len(self)
    }

    fn file_lock<'a>(
        self: Box<Self>,
    ) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
        async { Err((IoError::NotSupported, self as _)) }.boxed()
    }

    unsafe fn unlock_unsafe(&self) -> IoResult<()> {
        async { Err(IoError::NotSupported) }.boxed()
    }
}

impl SysfsFile for Class {}

impl Read<u8> for Class {
    fn read<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        _: u64,
        mut buff: DmaBuff<'b>,
    ) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
        async {
            let b = unsafe { &mut *buff.data_ptr() };
            let class = self.accessor.ctl.lock().await.class;
            let len = b.len().min(class.len());
            b[..len].copy_from_slice(&class[..len]);
            Ok((buff, len))
        }
        .boxed()
    }
}

impl Write<u8> for Class {
    fn write<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        _: u64,
        buff: DmaBuff<'b>,
    ) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
        async { Err((IoError::ReadOnly, buff, 0)) }.boxed()
    }
}

#[file]
#[derive(Clone)]
struct ConfigRegionFile {
    accessor: alloc::sync::Arc<FunctionAccessor>,
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

    fn len(&self) -> IoResult<u64> {
        async { Ok(4096) }.boxed()
    }
}

impl NormalFile for ConfigRegionFile {
    fn len_chars(&self) -> IoResult<u64> {
        File::len(self)
    }

    fn file_lock<'a>(
        self: Box<Self>,
    ) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
        async { Err((IoError::NotSupported, self as _)) }.boxed()
    }

    unsafe fn unlock_unsafe(&self) -> IoResult<()> {
        async { Err(IoError::Exclusive) }.boxed()
    }
}

impl SysfsFile for ConfigRegionFile {}

impl Read<u8> for ConfigRegionFile {
    fn read<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        pos: u64,
        mut buff: DmaBuff<'b>,
    ) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
        async move {
            let tgt: &[u8] = &self.accessor.ctl.lock().await.cfg_region[pos as usize..];
            let b = unsafe { &mut *buff.data_ptr() };
            let len = tgt.len().min(buff.len());
            b[..len].copy_from_slice(&tgt[..len]);
            Ok((buff, len))
        }
        .boxed()
    }
}

impl Write<u8> for ConfigRegionFile {
    fn write<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        _: u64,
        buff: DmaBuff<'b>,
    ) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
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
