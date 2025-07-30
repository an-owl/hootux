use alloc::alloc::handle_alloc_error;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::alloc::{Allocator, Layout};
use core::any::Any;
use futures_util::FutureExt;
use hootux::fs::file::*;
use hootux::fs::sysfs::SysfsFile;
use hootux::fs::sysfs::{SysfsDirectory, bus::BusDeviceFile};
use hootux::fs::vfs::MajorNum;
use hootux::fs::{IoError, IoResult};

#[file]
#[derive(Clone)]
pub(crate) struct EhciFileContainer {
    inner: Arc<async_lock::Mutex<super::Ehci>>,
    major: MajorNum,
}

impl EhciFileContainer {
    /// Initialises a new instance EHCI instance.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `address` contains the physical address of an unbound EHCI controller.
    /// `layout` must describe the entire EHCI BAR. `bind` must contain the lock of the binding file
    /// that the other arguments describe.
    /// The binding file can be found at `/sys/bus/pci/{PCI_ADDR}/bind`
    pub(crate) async unsafe fn new(
        pci: Arc<async_lock::Mutex<hootux::system::pci::DeviceControl>>,
        bind: LockedFile<u8>,
        address: u32,
        layout: Layout,
    ) -> Self {
        // SAFETY: Must be asserted by the caller to be correct
        let alloc = unsafe { hootux::alloc_interface::MmioAlloc::new(address as usize) };
        let Ok(region) = alloc.allocate(layout) else {
            handle_alloc_error(layout)
        };
        // SAFETY: region is guaranteed by MmioAlloc to point to `address`, and the rest of the args must be guaranteed by the caller
        let mut ehci = unsafe { super::Ehci::new(region, address, layout, bind, pci).unwrap() };
        ehci.configure().await;
        ehci.controller_enable(false);
        ehci.execute_async(true);
        ehci.execute_periodic(true);

        hootux::task::util::sleep(2).await;
        if ehci.is_enabled() {
            let timeout: hootux::time::AbsoluteTime = hootux::time::Duration::millis(2).into();
            log::trace!("EHCI not disabled after deadline");
            while ehci.is_enabled() {
                core::hint::spin_loop();
                if timeout.is_future() {
                    panic!("EHCI took wayyyy too long to halt")
                }
            }
        }
        ehci.controller_enable(true);

        let this = Self {
            major: ehci.major_num,
            inner: Arc::new(async_lock::Mutex::new(ehci)),
        };

        super::Ehci::start_port_watchdog(&this.inner).await;
        this
    }
}

impl File for EhciFileContainer {
    fn file_type(&self) -> FileType {
        FileType::Directory
    }

    fn block_size(&self) -> u64 {
        hootux::mem::PAGE_SIZE as u64
    }

    fn device(&self) -> DevID {
        // we are the root device, all devices on the bus use their USB bus address as minor.
        DevID::new(self.major, 0)
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        self.device().as_int().1 as u64
    }

    fn len(&self) -> IoResult<u64> {
        async { Ok(SysfsDirectory::entries(self) as u64) }.boxed()
    }
}

impl SysfsDirectory for EhciFileContainer {
    fn entries(&self) -> usize {
        todo!()
    }

    fn file_list(&self) -> Vec<String> {
        todo!()
    }

    fn get_file(&self, _name: &str) -> Result<Box<dyn SysfsFile>, IoError> {
        todo!()
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

impl BusDeviceFile for EhciFileContainer {
    fn bus(&self) -> &'static str {
        "usb"
    }

    fn id(&self) -> String {
        alloc::format!("usb{}", self.device().as_int().0)
    }

    fn as_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}
