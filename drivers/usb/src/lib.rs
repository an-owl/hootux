#![no_std]
#![feature(allocator_api)]
extern crate alloc;

use core::pin::Pin;
use futures_util::FutureExt;
use hootux::task::TaskResult;

const PAGE_SIZE: usize = 4096;

const UHCI_PCI_CLASS: [u8; 3] = [0xc, 0x3, 0x00];
const OHCI_PCI_CLASS: [u8; 3] = [0xc, 0x3, 0x10];
const EHCI_PCI_CLASS: [u8; 3] = [0xc, 0x3, 0x20];
const EHCI_BAR: u8 = 0;
const XHCI_PCI_CLASS: [u8; 3] = [0xc, 0x3, 0x30];

static DEV_ID: hootux::fs::vfs::DeviceIdDistributer = hootux::fs::vfs::DeviceIdDistributer::new();

pub mod ehci;

/// Represents a target device address. USB address are `0..128`
///
/// The address `0` is the default address which must be responded to.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
enum DeviceAddress {
    Default,
    Address(core::num::NonZeroU8),
}

impl DeviceAddress {
    /// Attempts to construct a new device from a `u8`.
    ///
    /// * Addresses bust be `<128`
    /// * `0` is the broadcast address
    const fn new(address: u8) -> Option<Self> {
        match address {
            0 => Some(DeviceAddress::Default),
            n @ 1..64 => Some(Self::Address(core::num::NonZeroU8::new(n).unwrap())), // `n` is not one
            _ => None,
        }
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
struct Endpoint {
    num: u8,
}

impl Endpoint {
    /// Constructs a new Endpoint.
    ///
    /// `num` must be less than 16.
    const fn new(num: u8) -> Option<Self> {
        match num {
            0..16 => Some(Endpoint { num }),
            _ => None,
        }
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
struct Target {
    pub dev: DeviceAddress,
    pub endpoint: Endpoint,
}

impl TryFrom<Target> for ::ehci::frame_lists::Target {
    type Error = ();
    fn try_from(value: Target) -> Result<Self, Self::Error> {
        // UEB 2.0 only allows addresses < 64
        // IDK about other revisions, they may allow more
        let addr_raw = match value.dev {
            DeviceAddress::Address(addr) if addr.get() >= 64 => return Err(()),
            DeviceAddress::Address(addr) => addr.get(),
            DeviceAddress::Default => 0,
        };

        Ok(Self {
            address: ::ehci::frame_lists::Address::new(addr_raw),
            endpoint: ::ehci::frame_lists::Endpoint::new(value.endpoint.num),
        })
    }
}

#[derive(Debug, Copy, Clone)]
pub enum PidCode {
    Control,
    In,
    Out,
}

impl From<::ehci::frame_lists::PidCode> for PidCode {
    fn from(code: ::ehci::frame_lists::PidCode) -> Self {
        use ::ehci::frame_lists::PidCode as Pid;
        match code {
            Pid::Out => PidCode::Out,
            Pid::In => PidCode::In,
            Pid::Setup => PidCode::Control,
        }
    }
}

impl From<PidCode> for ::ehci::frame_lists::PidCode {
    fn from(code: PidCode) -> Self {
        use ::ehci::frame_lists::PidCode as Pid;
        match code {
            PidCode::In => Pid::In,
            PidCode::Out => Pid::Out,
            PidCode::Control => Pid::Setup,
        }
    }
}

pub fn init() {
    hootux::task::run_task(init_async().boxed());
}

async fn init_async() -> TaskResult {
    use cast_trait_object::DynCastExt;
    use core::task::Poll;
    use hootux::fs::file::*;
    use hootux::fs::sysfs::*;
    loop {
        let event_file = cast_file!(NormalFile: SysfsDirectory::get_file(&mut SysFsRoot::new().bus,"event").unwrap()
            .dyn_upcast()).unwrap();
        let mut event = event_file.read(0, hootux::mem::dma::BogusBuffer::boxed());
        // Poll event to mark to setup wake event, to prevent missed events while we are working
        {
            while let Poll::Ready(_) = hootux::poll_once!(Pin::new(&mut event)) {
                event = event_file.read(0, hootux::mem::dma::BogusBuffer::boxed());
            }
        }

        let pci_dir = SysfsDirectory::get_file(&SysFsRoot::new().bus, "pci")
            .unwrap()
            .into_sysfs_dir()
            .unwrap();
        for i in SysfsDirectory::file_list(&*pci_dir) {
            let function = SysfsDirectory::get_file(&*pci_dir, &i)
                .unwrap()
                .into_sysfs_dir()
                .unwrap();
            let class =
                cast_file!(NormalFile: SysfsDirectory::get_file(&*function,"class").unwrap().dyn_upcast())
                    .unwrap();
            let mut buffer = [0u8, 0, 0];

            // SAFETY: sys/bus/pci/*/class is always synchronous PIO and can never be DMA.
            unsafe {
                class
                    .read(
                        0,
                        alloc::boxed::Box::new(hootux::mem::dma::StackDmaGuard::new(&mut buffer)),
                    )
                    .await
            }
            .ok()
            .unwrap(); // This will not return an error

            match buffer {
                UHCI_PCI_CLASS => log::info!("UHCI device found but no driver is implemented {i}"),
                OHCI_PCI_CLASS => log::info!("OHCI device found but no driver is implemented {i}"),
                EHCI_PCI_CLASS => {
                    let bind = cast_file!(NormalFile: SysfsDirectory::get_file(&*function,"bind").unwrap() as alloc::boxed::Box<dyn File>).unwrap();
                    let Ok(bind) = bind.file_lock().await else {
                        continue;
                    }; // already bound
                    let Ok(cfg_file) = SysfsDirectory::get_file(&*function, "cfg") else {
                        continue;
                    };
                    let result = cfg_file
                        .method_call("ctl_raw", &())
                        .await
                        .unwrap()
                        .inner
                        .downcast::<alloc::sync::Arc<async_lock::Mutex<hootux::system::pci::DeviceControl>>>().unwrap();
                    let cfg = result.lock_arc().await;

                    let bar_info = cfg.get_bar(EHCI_BAR).unwrap(); // Presence is guaranteed by the spec
                    let bar_addr = bar_info.addr().try_into().unwrap();
                    let bar_layout = bar_info.layout();
                    drop(cfg);
                    // SAFETY: This is safe because the the address and layout is guaranteed to be correct.
                    // Both values are fetched directly from the BAR
                    let ehci = unsafe {
                        ehci::file::EhciFileContainer::new(*result, bind, bar_addr, bar_layout)
                            .await
                    };

                    SysFsRoot::new()
                        .bus
                        .insert_device(alloc::boxed::Box::new(ehci));
                }

                XHCI_PCI_CLASS => log::info!("XHCI device found but no driver is implemented {i}"),
                _ => {} // no match
            }
        }
        let _ = event.await; // never returns err
    }
}
