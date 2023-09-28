use alloc::boxed::Box;
use core::alloc::Allocator;
use core::pin::Pin;
use hootux::system::driver_if::{MatchState, ResourceId};
use hootux::task::int_message_queue::MessageCfg;

// note: this is 1,6,1 (mass storage, sata, ahci 1.x)
const AHCI_CLASS: u32 = 0x01060100; // revision is masked.
const AHCI_HBA_BAR: u8 = 5;

// todo use better method for this.
// like benchmarking the device, and attempting to determine the average response rate and then half it.
const POLL_RATE: u64 = 100; // poll rate in Hz
const POLL_MSEC: u64 = 1000 / POLL_RATE;

pub struct AhciPciProfile;
impl hootux::system::driver_if::DriverProfile for AhciPciProfile {
    fn try_start(
        &self,
        resource: Box<dyn ResourceId>,
    ) -> (MatchState, Option<Box<dyn ResourceId>>) {
        if hootux::system::driver_if::WhoIsResource::whois(&*resource)
            == core::any::TypeId::of::<hootux::system::pci::PciResourceContainer>()
        {
            let pci_dev: Box<hootux::system::pci::PciResourceContainer> =
                match resource.as_any().downcast() {
                    Ok(res) => res,
                    Err(_) => unreachable!(), // TypeId checked above, this branch is impossible
                };

            let class = pci_dev.class() & !0xff; // mask revision bits

            return if class != AHCI_CLASS {
                (MatchState::NoMatch, Some(pci_dev))
            } else {
                if pci_dev.get_inner().lock().get_bar(AHCI_HBA_BAR).is_some() {
                    log::trace!(
                        "Attempting to start {} for {}",
                        crate::CRATE_NAME,
                        pci_dev.addr()
                    );
                    AhciDriver::start(pci_dev.get_inner()).unwrap(); // I dont get a whole lot of choice here but to panic
                                                                     // I can't return the resource as a PciResource
                    (MatchState::Success, None)
                } else {
                    log::error!("PCI device with class {AHCI_CLASS:#x} (AHCI) did not implement BAR 5; Poisoning device {}",pci_dev.addr());
                    (MatchState::MatchRejected, None)
                }
            };
        }
        (MatchState::WrongBus, Some(resource))
    }

    fn bus_name(&self) -> &str {
        "pci"
    }
}

#[allow(dead_code)]
// self.pci_dev required for future implementations
pub struct AhciDriver {
    name: &'static str,
    pci_dev: alloc::sync::Arc<spin::Mutex<hootux::system::pci::DeviceControl>>,
    hba: super::AbstractHba,
    wakeup: MessageDelivery,
    single_int: bool,
}

enum MessageDelivery {
    Polled,
    Int(hootux::task::int_message_queue::IntMessageQueue<hootux::system::pci::CfgIntResult>),
}

enum PollDev {
    All,
    One(u8),
}
impl MessageDelivery {
    async fn poll(&mut self) -> PollDev {
        match self {
            MessageDelivery::Polled => {
                hootux::task::util::sleep(POLL_MSEC).await;
                PollDev::All
            }
            MessageDelivery::Int(q) => {
                let (m, c) = Pin::new(q).poll_with_count().await;
                if c >= 32 {
                    PollDev::All
                } else {
                    // vector n corresponds to port n (except CCC)
                    PollDev::One(m.0 as u8)
                }
            }
        }
    }
}

impl AhciDriver {
    /// Initializes the driver using the PCI device. BAR 5 should be allocated by the caller.
    #[cold]
    fn start(
        pci_dev: alloc::sync::Arc<spin::Mutex<hootux::system::pci::DeviceControl>>,
    ) -> Result<(), &'static str> {
        let mut lock = pci_dev.lock();
        let b = lock.get_bar(AHCI_HBA_BAR).unwrap(); // In theory this should enver happen but it also checked by try_start just in case.

        // SAFETY: The address is given by the PCI bar and will and is marked as reserved
        let r = unsafe {
            &mut *hootux::alloc_interface::MmioAlloc::new(b.addr() as usize)
                .allocate(b.layout())
                .map_err(|_| "System ran out of memory")?
                .as_ptr()
        };
        let mut single;
        let md = if let Some(m_queue) =
            hootux::task::int_message_queue::IntMessageQueue::from_pci(&mut *lock, 32)
        // 32 is the max number of ints that should be used, I think the max is 1028 though\
        {
            let mut msi =
                hootux::system::pci::capabilities::msi::MessageSigInt::try_from(&mut *lock)
                    .expect("AHCI did not implement MSI"); // fixme: what about legacy ints
                                                           // SAFETY: Interrupts are handled by m_queue
            unsafe { msi.enable(true) }

            if m_queue.count() == 1 {
                single = true;
            } else {
                single = false;
            }
            MessageDelivery::Int(m_queue)
        } else {
            single = true;
            MessageDelivery::Polled
        };

        // SAFETY: This is definitely the BAR memory
        let hba = unsafe {
            super::AbstractHba::new(
                r.try_into().map_err(|_| "Incorrect BAR size")?,
                lock.address(),
            )
        }; // hol up, is the BAR the wrong size?

        if hba
            .general
            .lock()
            .get_control()
            .contains(crate::hba::general_control::GlobalHbaCtl::MSI_SINGLE_MESSAGE)
        {
            single = true;
            // todo reallocate IRQs
        }

        drop(lock);

        let use_int = if let MessageDelivery::Int(_) = md {
            true
        } else {
            false
        };

        let s = Box::new(Self {
            name: crate::CRATE_NAME,
            pci_dev,
            hba,
            wakeup: md,
            single_int: single,
        });

        s.init_blockdev();

        if use_int {
            for i in s.hba.ports.iter().filter_map(|p| p.as_ref()) {
                use crate::hba::port_control::InterruptEnable;

                let mut ints = InterruptEnable::DEVICE_MECHANICAL_PRESENCE
                    | InterruptEnable::COLD_PORT_DETECT
                    | InterruptEnable::INTERFACE_FATAL
                    | InterruptEnable::INTERFACE_NON_FATAL
                    | InterruptEnable::TASK_FILE_ERROR
                    | InterruptEnable::PORT_CONNECT_CHANGE
                    | InterruptEnable::DEV_TO_HOST_FIS
                    | InterruptEnable::PIO_SETUP_FIS
                    | InterruptEnable::DMA_SETUP_FIS;
                // SAFETY: This is safe because it only occurs if MSI was configured.
                unsafe {
                    // retries set_int_enable until all forbidden bits are cleared (should be max 2 tries)
                    while let Err(b) = i.set_int_enable(ints) {
                        ints.set(b, false);
                    }
                }
            }
            s.hba.int_enable(true);
        } else {
            s.hba.int_enable(false);
        }

        hootux::task::run_task(Box::pin(Box::new(s).run()));

        Ok(())
    }

    /// Initializes block devices
    fn init_blockdev(&self) {
        for p in self.hba.ports.iter().flat_map(|p| p) {
            p.cfg_blkdev();
        }
    }

    /// Actually runs the driver. Receives ints and dispatches self accordingly
    async fn run(mut self: Box<Self>) {
        loop {
            match self.wakeup.poll().await {
                PollDev::All => self.hba.chk_ports().await,
                PollDev::One(n) => {
                    if self.single_int {
                        self.hba.chk_ports().await;
                    } else {
                        self.hba.ports[n as usize]
                            .as_ref()
                            .expect("Attempted to poll invalid port")
                            .update()
                            .await;
                    }
                }
            }
        }
    }
}

impl Drop for AhciDriver {
    fn drop(&mut self) {
        self.hba.int_enable(false);
        match &mut self.wakeup {
            MessageDelivery::Polled => {}
            MessageDelivery::Int(d) => {
                // ints disabled above
                unsafe { d.drop_irq() }
            }
        }
    }
}
