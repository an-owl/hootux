use crate::PortState;
use crate::driver::{CmdErr, Port};
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::Any;
use futures::FutureExt;
use futures::future::BoxFuture;
use hootux::fs::sysfs::{SysfsDirectory, SysfsFile};
use hootux::fs::{IoError, IoResult, file::*};
use hootux::mem::dma::DmaBuff;

static ID_PROVIDER: hootux::fs::vfs::DeviceIdDistributer =
    hootux::fs::vfs::DeviceIdDistributer::new();

#[file]
pub(super) struct ControllerDir {
    id: DevID,
    pci_id: String,
    pci: Box<dyn Directory>,
    ports: [Option<AhciCharDevice>; 32],
}

impl ControllerDir {
    pub(super) async fn init(ahci: &super::kernel_if::AhciDriver) -> Result<(), ()> {
        let port_iter = ahci.get_ports().map(|o| o.map(|a| a.clone()));
        let mut ports = [const { None }; 32];
        // iterates over all ports where the port is present
        for (i, port) in port_iter.enumerate().filter(|(_, port)| port.is_some()) {
            if let Some(ref port) = port {
                ports[i] = Some(AhciCharDevice {
                    id: ID_PROVIDER.alloc_id(),
                    port: port.clone(),
                    locked: Default::default(),
                });
            }
        }

        let func = ahci.pci_function();
        let Ok(pci) = func.get_file("..").await else {
            panic!("The fuck? sys/bus/pci disappeared?")
        };

        let pci_addr = match get_file_name(&*cast_file!(Directory: pci).ok().unwrap(), &*func).await
        {
            Ok(name) => name,
            Err(IoError::NotPresent) => return Err(()),
            _ => unreachable!(),
        };

        let this = Self {
            id: ID_PROVIDER.alloc_id(),
            pci_id: pci_addr,
            pci: cast_file!(Directory: ahci.pci_function().clone_file()).unwrap(),
            ports,
        };

        this.query_ports().await; // Get identity for all ports.

        hootux::fs::sysfs::SysFsRoot::new()
            .bus
            .insert_device(Box::new(this.clone()));

        Ok(())
    }

    async fn query_ports(&self) {
        let iter = self
            .ports
            .iter()
            .flatten()
            .filter(|d| d.get_state() > PortState::None)
            .map(|d| d.get_ident());
        futures::future::join_all(iter).await;
    }
}

impl Clone for ControllerDir {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            pci_id: self.pci_id.clone(),
            pci: cast_file!(Directory: self.pci.clone_file()).unwrap(),
            ports: self.ports.clone(),
        }
    }
}

impl File for ControllerDir {
    fn file_type(&self) -> FileType {
        FileType::Directory
    }

    fn block_size(&self) -> u64 {
        hootux::mem::PAGE_SIZE as u64
    }

    fn device(&self) -> DevID {
        self.id
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        0
    }

    fn len(&self) -> IoResult<'_, u64> {
        async { Ok(SysfsDirectory::entries(self) as u64) }.boxed()
    }
}

impl SysfsDirectory for ControllerDir {
    fn entries(&self) -> usize {
        self.ports.iter().flatten().count()
    }

    fn file_list(&self) -> Vec<String> {
        let mut file_list: Vec<String> = Vec::new();
        for (i, _) in self
            .ports
            .iter()
            .enumerate()
            .filter(|(_, p)| p.as_ref().is_some_and(|p| p.get_state() != PortState::None))
        {
            // NotImplemented is implicit here, we can only get the port is it is implemented
            file_list.push(alloc::format!("port{i}"));
        }
        file_list
    }

    /// This has an annoying quirk. This is not async but [File::block_size] is also not async.
    /// At this point we may not know what the devices geometry is, and we do not have the time to check.
    /// As a result this may return [IoError::NotReady] when the geometry is not known and start a task to fetch the identity.
    fn get_file(&self, name: &str) -> Result<Box<dyn SysfsFile>, IoError> {
        match name {
            _ if name.starts_with("port") => {
                let Ok(port_n): Result<usize, _> = str::parse(name.strip_prefix("port").unwrap())
                // unwrap because we've checked this starts with "port"
                else {
                    return Err(IoError::NotPresent);
                };
                let Some(Some(port)) = self.ports.get(port_n) else {
                    return Err(IoError::NotPresent);
                };
                if port.get_state() != PortState::None {
                    if port.identity().is_some() {
                        Ok(Box::new(port.clone()))
                    } else {
                        let pclone = port.clone();
                        hootux::task::run_task(Box::pin(async move {
                            pclone.get_ident().await;
                            hootux::task::TaskResult::ExitedNormally
                        }));
                        Err(IoError::NotReady)
                    }
                } else {
                    Err(IoError::NotPresent)
                }
            }
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

impl hootux::fs::sysfs::bus::BusDeviceFile for ControllerDir {
    fn bus(&self) -> &'static str {
        "ahci"
    }
    fn id(&self) -> String {
        self.pci_id.to_string()
    }
    fn as_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[file]
#[derive(Clone)]
struct AhciCharDevice {
    id: DevID,
    port: alloc::sync::Arc<Port>,
    locked: alloc::sync::Arc<hootux::util::Weak<dyn NormalFile<u8>>>, // looks wierd. We must share the weak because its shared by all FOs for this device.
}

impl AhciCharDevice {
    fn identity(&self) -> Option<crate::driver::DevIdentity> {
        *self.port.identity.lock()
    }

    async fn get_ident(&self) -> crate::driver::DevIdentity {
        self.port.get_identity().await
    }

    async fn len_bytes(&self) -> u64 {
        let id = self.get_ident().await;
        id.lba_size * id.lba_count
    }

    /// Checks that this file object is the owning file accessor and that the operation geometry is valid
    ///
    /// This may return a truncated length to prevent attempting to access LBAs that do not exist.
    async fn op_sanity_check(&self, pos: u64, len: usize) -> Result<usize, IoError> {
        let mut len = len as u64;
        if !self.locked.cmp_t(self) {
            return Err(IoError::Exclusive);
        }
        let id = self.get_ident().await;
        if pos % id.lba_size != 0 {
            return Err(IoError::InvalidData);
        }
        let this_len = self.len_bytes().await;
        if pos > this_len {
            return Err(IoError::EndOfFile);
        }
        if len % id.lba_size != 0 {
            return Err(IoError::InvalidData);
        }

        if len + pos > this_len {
            let off_end = (len + pos) - this_len; // get diff
            len = len - off_end; // sub by diff
        }

        Ok(len.try_into().unwrap())
    }

    fn get_state(&self) -> PortState {
        self.port.get_state()
    }
}

impl SysfsFile for AhciCharDevice {}

impl File for AhciCharDevice {
    fn file_type(&self) -> FileType {
        FileType::CharDev
    }

    fn block_size(&self) -> u64 {
        self.port.identity.lock().as_ref().unwrap().lba_size
    }

    fn device(&self) -> DevID {
        self.id
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        self.id.as_int().1 as u64
    }

    fn len(&self) -> IoResult<'_, u64> {
        async {
            // This can panic if the
            let id = self.port.get_identity().await;
            Ok(id.lba_size * id.lba_count)
        }
        .boxed()
    }
}

impl NormalFile for AhciCharDevice {
    fn len_chars(&self) -> IoResult<'_, u64> {
        File::len(self)
    }

    fn file_lock<'a>(
        self: Box<Self>,
    ) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
        async {
            let dupe = self.clone();
            let strong = hootux::util::SingleArc::new(self as _);
            match dupe.locked.set(&strong) {
                Ok(()) => Ok(LockedFile::new_from_lock(strong)),
                Err(_) => Err((IoError::Exclusive, strong.take())),
            }
        }
        .boxed()
    }

    unsafe fn unlock_unsafe(&self) -> IoResult<'_, ()> {
        async {
            self.locked.clear();
            Ok(())
        }
        .boxed()
    }
}

impl Read<u8> for AhciCharDevice {
    fn read<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        pos: u64,
        mut buff: DmaBuff<'b>,
    ) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
        async move {
            let sector_size = self.get_ident().await;
            let len = match self.op_sanity_check(pos, buff.len()).await {
                Ok(len) => len,
                Err(e) => return Err((e, buff, 0)),
            };

            let sector = crate::driver::SectorAddress::new(pos / sector_size.lba_size).unwrap(); // May panic if my maths is incorrect.
            match self.port.read(sector, buff).await {
                Ok(buff) => Ok((buff, len)),
                Err((buff, CmdErr::DevErr(b))) => {
                    log::error!("ATA command returned error with bits {b:?}");
                    Err((IoError::MediaError, buff, 0))
                }
                Err((buff, CmdErr::Disowned)) => {
                    log::info!("Device {} was removed while command was active", self.id);
                    Err((IoError::MediaError, buff, 0))
                }
                Err((buff, e)) => {
                    log::error!("Driver error: Command failed with {e:?}");
                    Err((IoError::DeviceError, buff, 0))
                }
            }
        }
        .boxed()
    }
}

impl Write<u8> for AhciCharDevice {
    fn write<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        pos: u64,
        mut buff: DmaBuff<'b>,
    ) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
        async move {
            let sector_size = self.get_ident().await;
            let len = match self.op_sanity_check(pos, buff.len()).await {
                Ok(len) => len,
                Err(e) => return Err((e, buff, 0)),
            };

            let sector = crate::driver::SectorAddress::new(pos / sector_size.lba_size).unwrap(); // May panic if my maths is incorrect.
            match self.port.write(sector, buff).await {
                Ok(buff) => Ok((buff, len)),
                Err((buff, CmdErr::DevErr(b))) => {
                    log::error!("ATA command returned error with bits {b:?}");
                    Err((IoError::MediaError, buff, 0))
                }
                Err((buff, CmdErr::Disowned)) => {
                    log::info!("Device {} was removed while command was active", self.id);
                    Err((IoError::MediaError, buff, 0))
                }
                Err((buff, e)) => {
                    log::error!("Driver error: Command failed with {e:?}");
                    Err((IoError::DeviceError, buff, 0))
                }
            }
        }
        .boxed()
    }
}
