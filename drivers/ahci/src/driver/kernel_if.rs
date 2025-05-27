use crate::PortState;
use alloc::boxed::Box;
use cast_trait_object::DynCastExt;
use core::alloc::Allocator;
use core::pin::Pin;
use hootux::task::int_message_queue::MessageCfg;

const AHCI_HBA_BAR: u8 = 5;

// todo use better method for this.
// like benchmarking the device, and attempting to determine the average response rate and then half it.
const POLL_RATE: u64 = 100; // poll rate in Hz
const POLL_MSEC: u64 = 1000 / POLL_RATE;

#[allow(dead_code)]
// self.pci_dev required for future implementations
pub struct AhciDriver {
    pci_dev: alloc::sync::Arc<async_lock::Mutex<hootux::system::pci::DeviceControl>>,
    hba: super::AbstractHba,
    wakeup: MessageDelivery,
    single_int: bool,
    pci_file: Box<dyn hootux::fs::file::Directory>,
    binding: hootux::fs::file::LockedFile<u8>,
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
    pub(super) async fn start(
        func_root: Box<dyn hootux::fs::file::File>,
        fn_binding: hootux::fs::file::LockedFile<u8>,
    ) -> Result<(), &'static str> {
        use hootux::fs::file::*;
        // method_call always succeeds

        let function_dir = cast_file!(Directory: func_root).unwrap(); // always a directory
        let Ok(config) = function_dir.get_file("cfg").await else {
            return Err("Failed to get PCI function class file");
        };
        let config = cast_file!(NormalFile: config).unwrap();

        let Ok(pci_dev): Result<
            Box<alloc::sync::Arc<async_lock::Mutex<hootux::system::pci::DeviceControl>>>,
            _,
        > = config
            .method_call("ctl_raw", &())
            .await
            .unwrap()
            .inner
            .downcast()
        else {
            return Err("Failed to downcast after calling `ctl_raw`");
        };

        let pci_dev = *pci_dev; // drop the box

        let mut lock = pci_dev.lock().await;
        let b = lock.get_bar(AHCI_HBA_BAR).unwrap(); // In theory this should enver happen but it also checked by try_start just in case.

        // SAFETY: The address is given by the PCI bar and will and is marked as reserved
        let r = unsafe {
            &mut *hootux::alloc_interface::MmioAlloc::new(b.addr() as usize)
                .allocate(b.layout())
                .map_err(|_| "System ran out of memory")?
                .as_ptr()
        };
        let mut single;
        let md = match hootux::task::int_message_queue::IntMessageQueue::from_pci(&mut *lock, 32)
        // 32 is the max number of ints that should be used, I think the max is 1028 though\
        { Some(m_queue) => {
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
        } _ => {
            single = true;
            MessageDelivery::Polled
        }};

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
            pci_dev,
            hba,
            wakeup: md,
            single_int: single,
            pci_file: function_dir,
            binding: fn_binding,
        });

        let mut states = [PortState::NotImplemented; 32];
        for (i, port) in s.hba.ports.iter().enumerate() {
            states[i] = port
                .as_ref()
                .map(|p| p.get_state())
                .unwrap_or(PortState::NotImplemented)
        }
        log::trace!(
            "AHCI: {} ports enabled\n{states:?}\n",
            s.hba.ports.iter().flatten().count()
        );

        let Ok(()) = super::file::ControllerDir::init(&*s).await else {
            return Err("Failed to install AHCI into sysfs");
        };

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

    /// Returns an iterator over all active ports.
    pub(crate) fn get_ports(&self) -> impl Iterator<Item = Option<&alloc::sync::Arc<super::Port>>> {
        self.hba.ports.iter().map(|e| e.as_ref())
    }

    pub(crate) fn pci_function(&self) -> Box<dyn hootux::fs::file::Directory> {
        self.pci_file.clone_file().dyn_cast().unwrap()
    }

    /// Actually runs the driver. Receives ints and dispatches self accordingly
    async fn run(mut self: Box<Self>) -> hootux::task::TaskResult {
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
