use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use crate::fs::file::*;
use super::*;

// fixme make this a warning
#[cfg(target_pointer_width = "32")]
compile_error!("tmpfs can only store 4GiB files on 32bit systems");

pub struct TmpFs {
    root: Arc<Dir>
}

impl TmpFs {
    pub fn new() -> Self {
        // Null Weak causes get_file("..") to return self
        Self {
            root: Arc::new(Dir {
                map: Default::default(),
                parent: Weak::new(),
            })
        }
    }
}

impl Filesystem for TmpFs {
    fn root(&self) -> Box<dyn Directory> {
        if let FileTypeWrapper::Dir(f) = self.root.clone().get_file_obj().downcast() {
            f
        } else {
            unreachable!()
        }
    }
}

struct Dir {
    // This will only be locked while its traversed, so this will never spin too long.
    // Performance overhead will likely be smaller this way, it can always be changed later.
    map: spin::RwLock<alloc::collections::BTreeMap<String, Arc<dyn TmpFsFile>>>,
    parent: Weak<Self>,
}

trait TmpFsFile {
    fn get_file_obj(self: Arc<Self>) -> Box<dyn File>;
}

impl TmpFsFile for Dir {
    fn get_file_obj(self: Arc<Self>) -> Box<dyn File> {
        Box::new(DirRef { inner: Arc::downgrade(&self) })
    }
}

// SAFETY: Everything inside Dir is within some kind of wrapper
unsafe impl Send for Dir {}
unsafe impl Sync for Dir {}

#[derive(Clone)]
struct DirRef {
    inner: Weak<Dir>
}

impl File for DirRef {
    fn downcast(self: Box<Self>) -> FileTypeWrapper {
        FileTypeWrapper::Dir(self)
    }

    fn file_type(&self) -> IoResult<FileType> {
        async { Ok(FileType::Directory) }.boxed()
    }

    fn block_size(&self) -> u64 {
        1
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }
}

impl Directory for DirRef {
    fn len(&self) -> IoResult<usize> {
        async {
            let inner = self.inner.upgrade().ok_or(IoError::NotPresent)?;
            let len = inner.map.read().len();
            Ok(len)
        }.boxed()
    }

    fn new_file<'a>(&'a self, name: &'a str, file: Option<&'a mut dyn NormalFile<u8>>) -> BoxFuture<Result<(), (Option<IoError>, Option<IoError>)>> {
        async {
            let inner = self.inner.upgrade().ok_or((Some(IoError::NotPresent), None))?;
            if let Some(file) = file {
                // Lock the things

                let len = file.len().await.map_err(|e| (None, Some(e)))?;
                let mut vec = Vec::with_capacity(len.try_into().unwrap());

                // Extends the buffer without initializing memory
                // SAFETY: This is safe because we set the size above & u8 is always valid to read.
                unsafe { vec.set_len(len.try_into().unwrap()) };
                let _: &mut [u8] = file.read(&mut vec).await.map_err(|(e, _)| (None, Some(e)))?;

                inner.map.write().insert(
                    name.to_string(),
                    Arc::new(NormalFileNode {
                        data: async_lock::RwLock::new(vec),
                        lock: spin::Mutex::new(Default::default())
                    }
                    )
                ).ok_or((Some(IoError::NotPresent), None))?;
                Ok(())
            } else {
                inner.map.write().insert(name.to_string(), Arc::new(NormalFileNode::new()));
                Ok(())
            }
        }.boxed()
    }

    fn new_dir<'a>(&'a self, name: &'a str) -> IoResult<Box<dyn Directory>> {
        async {
            let inner = self.inner.upgrade().ok_or(IoError::NotPresent)?;
            let dir = Arc::new(Dir { map: Default::default(), parent: self.inner.clone() });
            inner.map.write().insert(name.to_string(), dir.clone());

            let w = Arc::downgrade(&inner);
            Ok(Box::new( DirRef{inner: w}) as Box<dyn Directory>)
        }.boxed()
    }

    fn get_file<'a>(&'a self, name: &'a str) -> IoResult<Option<Box<dyn File>>> {
        async move {
            let inner = self.inner.upgrade().ok_or(IoError::NotPresent)?;
            let r = match name {
                "."  => {
                    self.inner.upgrade().ok_or(IoError::NotPresent)?;
                    Ok(Some(self.clone_file()))
                },
                ".." => {
                    let inner = self.inner.upgrade().ok_or(IoError::NotPresent)?;
                    if let Some(parent) = inner.parent.upgrade() {
                        Ok(Some(Box::new(DirRef{inner: Arc::downgrade(&parent)}) as Box<dyn File>))
                    } else {
                        self.inner.upgrade().ok_or(IoError::NotPresent)?;
                        Ok(Some(self.clone_file()))
                    }
                }
                f_name => {
                    if let Some(f) = inner.map.read().get(f_name) {
                        let fo = f.clone().get_file_obj();
                        Ok(Some(fo))
                    } else {
                        Ok(None)
                    }
                }
            };
            r
        }.boxed()
    }

    fn file_list(&self) -> IoResult<Vec<String>> {
        async {
            let inner = self.inner.upgrade().ok_or(IoError::NotPresent)?;
            let map = inner.map.read();
            let mut keys: Vec<String> = map.keys().map(|s| s.clone()).collect();
            keys.push("..".into());
            keys.push(".".into());
            Ok(keys)
        }.boxed()
    }
}


struct FileRef {
    file: Weak<NormalFileNode>,
    cursor: atomic::Atomic<u64>,
}

impl File for FileRef {
    fn downcast(self: Box<Self>) -> FileTypeWrapper {
        FileTypeWrapper::Normal(self)
    }

    fn file_type(&self) -> IoResult<FileType> {
        async {
            Ok(FileType::NormalFile)
        }.boxed()
    }

    fn block_size(&self) -> u64 {
        1
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(
            Self {
                file: self.file.clone(),
                cursor: atomic::Atomic::new(0),
            }
        )
    }
}

impl Read<u8> for FileRef {
    fn read<'a>(&'a mut self, buff: &'a mut [u8]) -> BoxFuture<Result<&'a mut [u8], (IoError, usize)>> {
        async {
            let inner = self.file.upgrade().ok_or((IoError::NotPresent,0))?;
            inner.is_lock(self).map_err(|e| (e,0))?;

            let file = inner.data.read().await;
            let read_start = self.cursor.load(atomic::Ordering::Relaxed) as usize;
            let len = buff.len().min(file.len() - read_start);

            buff[..len].copy_from_slice(&file[read_start..read_start+len]);
            drop(file);
            self.cursor.store(len as u64, atomic::Ordering::Release);
            Ok(&mut buff[..len])
        }.boxed()
    }
}

impl Write<u8> for FileRef {
    fn write<'a>(&'a mut self, buff: &'a [u8]) -> IoResult<usize> {
        async {
            let inner = self.file.upgrade().ok_or(IoError::NotPresent)?;
            inner.is_lock(self)?;

            let mut file = inner.data.write().await;
            let last_write = self.cursor.load(atomic::Ordering::Relaxed) as usize + buff.len();
            if last_write > file.len() {
                file.reserve(last_write);
                // SAFETY: Space for this len is reserved above.
                unsafe { file.set_len(last_write) }
            };
            file[self.cursor.load(atomic::Ordering::Relaxed) as usize..last_write].copy_from_slice(buff);
            drop(file);
            self.cursor.fetch_add(buff.len() as u64,atomic::Ordering::Release);

            Ok(buff.len())
        }.boxed()
    }
}

impl Seek for FileRef {
    fn set_cursor(&mut self, pos: u64) -> Result<u64, IoError> {
        self.cursor.store(pos, atomic::Ordering::Relaxed);
        Ok(pos)
    }

    fn rewind(&mut self, pos: u64) -> Result<u64, IoError> {
        self.cursor.fetch_sub(pos,atomic::Ordering::Relaxed);
        Ok(pos)
    }

    fn seek(&mut self, pos: u64) -> Result<u64, IoError> {
        self.cursor.fetch_add(pos,atomic::Ordering::Relaxed);
        Ok(pos)
    }
}

impl NormalFile<u8> for FileRef {
    fn len(&self) -> IoResult<u64> {
        async {
            // tasty one-liner
            Ok(self.file.upgrade().ok_or(IoError::NotPresent)?.data.read().await.len() as u64)
        }.boxed()
    }

    fn file_lock<'a>(self: Box<Self>) -> BoxFuture<'a, Result<LockedFile<u8>,(IoError,Box<dyn NormalFile<u8>>)>> {
        async {
            let file = if let Some(file) = self.file.upgrade() {
                file
            } else {
                return Err((IoError::Exclusive,self as Box<dyn NormalFile<u8>>))
            };

            let mut l = file.lock.lock();
            let this = crate::util::SingleArc::new(self as Box<dyn NormalFile<u8>>);
            if let None = l.get() {
                 *l = this.downgrade();
                Ok(LockedFile::new_from_lock(this))
            } else {
                return Err((IoError::Exclusive, this.take()))
            }


        }.boxed()
    }

    unsafe fn unlock_unsafe(&self) -> IoResult<()> {
        async {
            let accessor = self.file.upgrade().ok_or(IoError::NotPresent)?;
            let mut lock = accessor.lock.lock();
            lock.clear();
            Ok(())
        }.boxed()
    }
}


/// NormalFile accessor
struct NormalFileNode {
    data: async_lock::RwLock<Vec<u8>>,
    lock: spin::Mutex<crate::util::Weak<dyn NormalFile<u8>>>,
}

impl TmpFsFile for NormalFileNode {
    fn get_file_obj(self: Arc<Self>) -> Box<dyn File> {
        Box::new( FileRef{ file: Arc::downgrade(&self), cursor: Default::default() } )
    }
}

impl NormalFileNode {

    fn new() -> Self {
        Self {
            data: Default::default(),
            lock: spin::Mutex::new(crate::util::Weak::default())
        }
    }
    /// Checks if `file` is the one that currently locks self,
    /// returns `Ok(())` if it is returns `Err(IoError::Exclusive)` if it isn't
    fn is_lock(&self, file: &dyn NormalFile<u8>) -> Result<(),IoError> {
        let mut l = self.lock.lock();
        if let Some(ptr) = l.get() {
            if !core::ptr::eq(ptr, file) {
                return Err(IoError::Exclusive)
            }
        } else {
            l.clear();
        }
        Ok(())
    }
}

unsafe impl Send for NormalFileNode {}
unsafe impl Sync for NormalFileNode {}