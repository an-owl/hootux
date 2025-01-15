use super::*;
use super::file::*;
use alloc::{
    collections::BTreeMap,
    string::String,
    boxed::Box,
    sync::Weak,
    sync::Arc,
};
use alloc::vec::Vec;
use futures_util::future::BoxFuture;
use super::vfs::*;
use alloc::string::ToString;
use core::any::TypeId;
use cast_trait_object::DynCastExt;
use futures_util::FutureExt;
use lazy_static::lazy_static;
use crate::fs::IoError::NotPresent;
use crate::mem::dma::{DmaBuff, DmaClaimable, DmaTarget};

lazy_static! {
    pub static ref DRIVER_MAJOR: MajorNum = MajorNum::new();
}

static MINOR: atomic::Atomic<usize> = atomic::Atomic::new(0);

trait TmpFsFile: Sync + Send {
    fn get_file_obj(self: Arc<Self>, fs: Weak<TmpFsRootInner>) -> Box<dyn File>;

    fn link_count(&self) -> u64 {
        1
    }

    fn set_link(&self, _count: u64) {}

    fn type_id(&self) -> core::any::TypeId;
}

struct TmpFsRootInner {
    f_map: spin::RwLock<BTreeMap<u64, Arc<dyn TmpFsFile>>>,
    fs_opts: spin::RwLock<FsOpts>,
    dev_id: DevID,
    serial_count: atomic::Atomic<u64>,
}

impl TmpFsRootInner {
    fn new_file(&self) -> Option<Arc<FileAccessor>> {
        let serial = self.serial_count.fetch_add(1,atomic::Ordering::Relaxed);
        let file = Arc::new(FileAccessor::new(serial));
        if self.f_map.write().insert(serial,file.clone()).is_some() {
            None
        } else {
            Some(file)
        }
    }

    fn new_dir(&self,parent: &DirAccessor) -> Option<Arc<DirAccessor>> {
        let serial = self.serial_count.fetch_add(1,atomic::Ordering::Relaxed);
        let file = Arc::new(DirAccessor::new(serial,parent.serial));
        if self.f_map.write().insert(serial,file.clone()).is_some() {
            None
        } else {
            Some(file)
        }
    }

    /// Attempts to remove the file with the ID `serial`. If the file has multiple links to id then
    /// the link count is decremented.
    fn remove_file(&self, serial: u64) -> Result<(),IoError> {
        let mut l = self.f_map.write();
        if let Some(t) = l.get_mut(&serial) {
            let count = t.link_count();
            if t.link_count() > 1 {
                t.set_link(count - 1);
                Ok(())
            } else {
                let f = l.remove(&serial);
                // Explicitly set drop order.
                // if `f` is a directory, when it is dropped it recursively calls this fn
                // so the lock ust be dropped to prevent deadlocks.
                drop(l);
                drop(f);
                Ok(())

            }
        } else {
            Err(IoError::NotPresent)
        }
    }

    fn store_dev(&self, dev: Box<dyn device::DeviceFile>) -> u64 {
        let id = self.serial_count.fetch_add(1, atomic::Ordering::Relaxed);
        let dev = Arc::new(DeviceFileObj {
            inner: dev,
            _serial: id,
            link_count: atomic::Atomic::new(1),
        });

        // map drops the returned file because it does not implement Debug which is required by expect_err::<Result<Debug,_>>()
        let _ = self.f_map.write().insert(id,dev).map(|_|()).ok_or(()).expect_err("Duplicate file serial number");
        id
    }

    fn fetch(self: &Arc<Self>, id: u64) -> Option<Box<dyn File>> {
        self.f_map.read().get(&id).map(|d| d.clone().get_file_obj(Arc::downgrade(&self)))
    }

    /// Fetches the raw `dyn TmpFsFile` This should be dropped or downgraded as soon as possible.
    fn fetch_raw(&self, id: u64) -> Option<Arc<dyn TmpFsFile>> {
        self.f_map.read().get(&id).map(|d| d.clone())
    }
}

#[derive(Clone)]
#[cast_trait_object::dyn_cast(File => NormalFile<u8>, Directory, super::device::FileSystem, super::device::Fifo<u8>, super::device::DeviceFile )]
#[cast_trait_object::dyn_upcast(File)]
pub struct TmpFsRoot {
    inner: Arc<TmpFsRootInner>
}

impl TmpFsRoot {
    pub fn new() -> Box<dyn device::FileSystem> {

        let this = Box::new( Self {
            inner: Arc::new(TmpFsRootInner{
                f_map: spin::RwLock::new(BTreeMap::new()),
                fs_opts: spin::RwLock::new(FsOpts::new(true,true)),
                dev_id: DevID::new(*DRIVER_MAJOR, MINOR.fetch_add(1,atomic::Ordering::Relaxed)),
                serial_count: atomic::Atomic::new(1), // this file is 0
            })
        });
        let mut l = this.inner.f_map.write();
        let root = DirAccessor::new(0,0); // special exception parent of root has itself as parent
        l.insert(0,Arc::new(root));
        drop(l);

        this
    }
}

impl File for TmpFsRoot {
    fn file_type(&self) -> FileType {
        FileType::Directory
    }

    /* todo should the block size be
     - 1
     - cache line size
     - memory data width
     - usize
     - page size (not huge)
     */

    fn block_size(&self) -> u64 {
        crate::mem::PAGE_SIZE as u64 // Page flipping can be used when block size is 4K
    }

    fn device(&self) -> DevID {
        self.inner.dev_id
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        0
    }

    fn len(&self) -> IoResult<u64> {

        async {
            let t = self.inner.fetch(0).unwrap(); // 0 is always present
            t.len().await
        }.boxed()
    }
}

impl device::DeviceFile for TmpFsRoot {}

impl device::FileSystem for TmpFsRoot {
    fn root(&self) -> Box<dyn Directory> {
        cast_file!(Directory: self.inner.fetch(0).unwrap()).ok().unwrap() // root will always be a file
    }

    fn get_opt(&self, option: &str) -> Option<FsOptionVariant> {
        let l = self.inner.fs_opts.read();
        l.get(option)
    }

    fn set_opts(&mut self, options: &str) {
        let mut new_opts = FsOpts::new(false,true);
        for i in options.split_whitespace() {
            match i {
                "NODEV" => { new_opts.set(FsOpts::DEV_ALLOWED.to_string(), FsOpts::FALSE.to_string()); }
                "NOCACHE" => log::trace!("NOCACHE passed to tmpfs, ignoring"),
                e => log::warn!(r#"Unknown option "{e}" will be ignored"#)
            }
        }

        *self.inner.fs_opts.write() = new_opts;
    }

    fn driver_name(&self) -> &'static str {
        "tmpfs"
    }

    fn raw_file(&self) -> Option<&str> {
        None
    }
}

struct DirAccessor {
    map: spin::RwLock<BTreeMap<String,u64>>,
    parent: u64,
    serial: u64
}

impl DirAccessor {

    fn new(serial: u64, parent: u64) -> Self {
        Self {
            map: Default::default(),
            parent,
            serial,
        }
    }

    fn is_root(&self) -> bool {
        self.parent == self.serial
    }
}

impl TmpFsFile for DirAccessor {
    fn get_file_obj(self: Arc<Self>, fs: Weak<TmpFsRootInner>) -> Box<dyn File> {
        Box::new(Dir {
            accessor: self.clone(),
            fs,
            serial: self.serial
        })
    }

    fn type_id(&self) -> TypeId {
        TypeId::of::<Self>()
    }
}

#[derive(Clone)]
#[cast_trait_object::dyn_cast(File => NormalFile<u8>, Directory, super::device::FileSystem, super::device::Fifo<u8>, super::device::DeviceFile )]
#[cast_trait_object::dyn_upcast(File)]
struct Dir {
    accessor: Arc<DirAccessor>,
    fs: Weak<TmpFsRootInner>,
    serial: u64,
}

impl File for Dir {
    fn file_type(&self) -> FileType {
        FileType::Directory
    }

    fn block_size(&self) -> u64 {
        crate::mem::PAGE_SIZE as u64
    }

    fn device(&self) -> DevID {
        let fs = self.fs.upgrade().unwrap();
        fs.dev_id

    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        self.serial
    }

    fn len(&self) -> IoResult<u64> {
        async {
            let b = self.accessor.map.read();
            Ok(b.len() as u64)
        }.boxed()
    }
}

impl Directory for Dir {
    fn entries(&self) -> IoResult<usize> {
        async  {
            let l = self.accessor.map.read();
            Ok(l.len())
        }.boxed()
    }

    fn new_file<'s, 'a:'s>(&'s self, name: &'a str, file: Option<&'a mut dyn NormalFile<u8>>) -> BoxFuture<Result<(), (Option<IoError>, Option<IoError>)>> {
        async {

            let mut l = self.accessor.map.write();
            if let alloc::collections::btree_map::Entry::Vacant(entry) = l.entry(name.to_string()) {

                let fs = self.fs.upgrade().ok_or((Some(IoError::NotPresent),None))?;
                let new_file = fs.new_file().unwrap(); // im really not sure what to do if this occurs

                if let Some(file) = file {
                    let len = file.len_chars().await.map_err(|e| (None, Some(e)))?;
                    // fixme if another file appends to the file before reading then we do not capture the entire file.
                    let mut vec = Vec::with_capacity(len.try_into().unwrap());

                    // Extends the buffer without initializing memory
                    // SAFETY: This is safe because we set the size above & u8 is always valid to read.
                    unsafe { vec.set_len(len.try_into().unwrap()) };
                    let cursor = file.move_cursor(0).unwrap();
                    file.set_cursor(0).unwrap();
                    let dbuff = crate::mem::dma::DmaGuard::from(vec);
                    let (dbuff, claimed) = dbuff.claim().unwrap(); // cannot fail

                    let (_,read_len) = file.read(claimed).await.map_err(|(e, _, _)| (None, Some(e)))?; // drop claimed buffer after completion
                    let Ok(dbuff) = dbuff.unwrap() else { unreachable!() };
                    let mut vec = dbuff.unwrap();

                    vec.truncate(read_len); // If the file shrinks between getting len and reading then we truncate garbage data.

                    // restore cursor set it to 0 if we cant
                    if let Err(_) = file.set_cursor(cursor) {
                        file.set_cursor(0).unwrap();
                    }
                    *new_file.data.write().await = vec;
                }
                entry.insert(new_file.serial);
                Ok(())

            } else {
                Err((Some(IoError::AlreadyExists),None))
            }
        }.boxed()
    }

    fn new_dir<'a>(&'a self, name: &'a str) -> IoResult<Box<dyn Directory>> {
        async {

            let mut l = self.accessor.map.write();
            if let alloc::collections::btree_map::Entry::Vacant(entry) = l.entry(name.to_string()) {
                let fs = self.fs.upgrade().ok_or(IoError::NotPresent)?;
                let dir = fs.new_dir(&self.accessor).ok_or(IoError::DeviceError)?;
                entry.insert(dir.serial);
                //let t = cast_file!(Directory: dir.get_file_obj(self.fs.clone()).try_into()).unwrap();
                let f = dir.get_file_obj(self.fs.clone());
                let t: Box<dyn Directory> = f.dyn_cast().ok().unwrap(); // will never fail
                Ok(t) // Cast will always succeed
            } else {
                Err(IoError::AlreadyExists)
            }
        }.boxed()
    }

    fn store<'a>(&'a self, name: &'a str, file: Box<dyn File>) -> IoResult<()> {
        async {
            let mut l = self.accessor.map.write();
            if let alloc::collections::btree_map::Entry::Vacant(entry) = l.entry(name.to_string()) {

                match cast_file!(device::DeviceFile: file) {
                    Ok(device) => {
                        let id = self.fs.upgrade().unwrap().store_dev(device);
                        entry.insert(id);
                    }
                    Err(_) => {
                        log::warn!("Attempted to store() non device file");
                    }
                }
                Ok(())
            } else {
                Err(IoError::AlreadyExists)
            }
        }.boxed()
    }

    fn get_file<'a>(&'a self, name: &'a str) -> IoResult<Box<dyn File>> {
        async move {

            if name == PARENT_DIR && self.accessor.is_root() {
                return Err(IoError::IsDevice)
            }

            let id = *self.accessor.map.read().get(name).ok_or(IoError::NotPresent)?;
            Ok(self.fs.upgrade().unwrap().fetch(id).ok_or_else(
                || {
                    log::error!("tmpfs bug: Directory contained file entry but filesystem did cont contain the requested file");
                    IoError::NotPresent
                }
            )?)

        }.boxed()
    }

    fn get_file_with_meta<'a>(&'a self, name: &'a str) -> IoResult<FileHandle> {
        async move {

            if name == PARENT_DIR && self.accessor.is_root() {
                return Ok(FileHandle::new_dev(FileMetadata::new_unknown()))
            }

            let id = *self.accessor.map.read().get(name).ok_or(IoError::NotPresent)?;
            if let Some(f) = self.fs.upgrade().unwrap().fetch_raw(id) {
                let mut dev_hint = false;
                if TmpFsFile::type_id(&*f) == TypeId::of::<DeviceFileObj>() {
                    dev_hint = true;
                }
                let file = f.get_file_obj(self.fs.clone()) as Box<dyn File>;
                let meta = FileMetadata::new_from_file(&*file).await?;

                Ok(FileHandle::new(file, dev_hint, meta))
            } else {
                Err(NotPresent)
            }
        }.boxed()
    }

    fn file_list(&self) -> IoResult<Vec<String>> {
        async {
            Ok(self.accessor.map.read().keys().map(|s| s.clone()).collect())
        }.boxed()
    }

    fn remove<'a>(&'a self, name: &'a str) -> IoResult<()> {
        async {
            let id = *self.accessor.map.read().get(name).ok_or(IoError::NotPresent)?;
            let fs = self.fs.upgrade().ok_or(IoError::NotPresent)?;
            let file = fs.fetch(id).ok_or(IoError::NotPresent)?;

            if file.file_type() == FileType::Directory && file.len().await? > 0 {
                return Err(IoError::NotEmpty)
            }
            fs.remove_file(id)?;
            Ok(())
        }.boxed()
    }
}

struct FileAccessor {
    data: async_lock::RwLock<Vec<u8>>,
    lock: spin::Mutex<crate::util::Weak<dyn NormalFile<u8>>>,
    serial: u64
}

impl FileAccessor {
    fn new(serial: u64) -> Self {
        Self {
            data: async_lock::RwLock::new(Vec::new()),
            lock: spin::Mutex::new(crate::util::Weak::default()),
            serial
        }
    }
}

impl TmpFsFile for FileAccessor{
    fn get_file_obj(self: Arc<Self>, fs: Weak<TmpFsRootInner>) -> Box<dyn File> {
        Box::new(TmpFsNormalFile{
            accessor: self.clone(),
            fs,
            serial: self.serial,
            cursor: 0,
        })
    }

    fn type_id(&self) -> TypeId {
        <Self as core::any::Any>::type_id(self)
    }
}


#[derive(Clone)]
#[cast_trait_object::dyn_cast(File => NormalFile<u8>, Directory, super::device::FileSystem, super::device::Fifo<u8>, super::device::DeviceFile )]
#[cast_trait_object::dyn_upcast(File)]
struct TmpFsNormalFile {
    accessor: Arc<FileAccessor>,
    fs: Weak<TmpFsRootInner>,
    serial: u64,
    // this is usize because the data is in a vec with a max size of usize::MAX.
    // If I ever reimplement this as a Physical Region Description then I should change this to u64
    // ^^ unlikely
    cursor: usize,
}

impl NormalFile<u8> for TmpFsNormalFile {
    fn len_chars(&self) -> IoResult<u64> {
        async { Ok(self.accessor.data.read().await.len() as u64) }.boxed()
    }

    fn file_lock<'a>(self: Box<Self>) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
        async {

            let b = self.accessor.clone();
            let mut l = b.lock.lock();
            if let None = l.get() {
                let s = crate::util::SingleArc::new(self as Box<dyn NormalFile<u8>>);
                l.set(&s);

                Ok(LockedFile::new_from_lock(s))
            } else {
                Err((IoError::Exclusive,self as Box<dyn NormalFile<u8>>))
            }

        }.boxed()
    }

    unsafe fn unlock_unsafe(&self) -> IoResult<()> {
        async { Ok(self.accessor.lock.lock().clear()) }.boxed()
    }
}

impl File for TmpFsNormalFile {
    fn file_type(&self) -> FileType {
        FileType::Directory
    }

    fn block_size(&self) -> u64 {
        crate::mem::PAGE_SIZE as u64
    }

    fn device(&self) -> DevID {
        self.fs.upgrade().unwrap().dev_id
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        self.serial
    }

    fn len(&self) -> IoResult<u64> {
        async {
            let l = self.accessor.data.read().await;
            Ok(l.len() as u64)
        }.boxed()
    }
}

impl Seek for TmpFsNormalFile {
    fn set_cursor(&mut self, pos: u64) -> Result<u64, IoError> {
        self.cursor = pos.try_into().map_err(|_| IoError::EndOfFile)?;
        Ok(self.cursor as u64)
    }

    fn rewind(&mut self, pos: u64) -> Result<u64, IoError> {
        let mut c = self.cursor as u64;
        c = c.checked_sub(pos).ok_or(IoError::EndOfFile)?;
        self.cursor = c as usize; // Truncation cannot occur here.
        Ok(c)
    }

    fn seek(&mut self, pos: u64) -> Result<u64, IoError> {
        let mut c = self.cursor as u64;
        c = c.checked_add(pos).ok_or(IoError::EndOfFile)?;
        self.cursor = c.try_into().map_err(|_| IoError::EndOfFile)?;
        Ok(c)
    }
}

impl Read<u8> for TmpFsNormalFile {
    fn read<'f, 'a: 'f,'b: 'f>(&'a mut self, mut dbuff: DmaBuff<'b>) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
        async {
            let buff = unsafe {&mut *crate::mem::dma::DmaTarget::as_mut(&mut *dbuff) };
            if !self.accessor.lock.lock().cmp_t(self) {
                return Err((IoError::Exclusive, dbuff, 0));
            }
            let file = self.accessor.data.read().await;
            if self.cursor >= file.len() {
                return Err((IoError::EndOfFile, dbuff, 0))
            }
            let count = buff.len().min(file.len() - self.cursor); // either selects the remaining `file` length or the entire `buff` length
            buff[..count].copy_from_slice(&file[self.cursor..self.cursor + count]);
            self.cursor += count;
            Ok((dbuff,count))
        }.boxed()
    }
}

impl Write<u8> for TmpFsNormalFile {
    fn write<'f, 'a: 'f,'b: 'f>(&'a mut self, mut dbuff: DmaBuff<'b>) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
        async {
            let buff = unsafe {&mut *DmaTarget::as_mut(&mut *dbuff) };
            if !self.accessor.lock.lock().cmp_t(self) {
                return Err((IoError::Exclusive, dbuff, 0))
            }
            let mut file = self.accessor.data.write().await;
            // extend file if necessary
            if buff.len() + self.cursor > file.len() {
                // SAFETY: new len is valid & u8 does not have an invalid state
                file.reserve_exact(buff.len() + self.cursor);
                unsafe { file.set_len(buff.len() + self.cursor) };
            }
            file[self.cursor..self.cursor + buff.len()].copy_from_slice(buff);

            Ok((dbuff, buff.len()))
        }.boxed()
    }
}

/// This is a file accessor for a device file.
struct DeviceFileObj {
    inner: Box<dyn super::device::DeviceFile>,
    _serial: u64,
    link_count: atomic::Atomic<u64>
}

impl TmpFsFile for DeviceFileObj {
    fn get_file_obj(self: Arc<Self>, _fs: Weak<TmpFsRootInner>) -> Box<dyn File> {
        self.inner.clone_file()
    }

    fn link_count(&self) -> u64 {
        self.link_count.load(atomic::Ordering::Relaxed)
    }

    fn set_link(&self, count: u64) {
        self.link_count.store(count,atomic::Ordering::Relaxed)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of::<Self>()
    }
}