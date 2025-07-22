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

pub mod ehci {
    use crate::{DeviceAddress, Endpoint, PAGE_SIZE, PidCode, Target};
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use bitfield::{Bit, BitMut};
    use core::alloc::Allocator;
    use core::ptr::NonNull;
    use derivative::Derivative;
    use ehci::frame_lists::{QueueElementTransferDescriptor, QueueHead};
    use ehci::operational_regs::OperationalRegistersVolatileFieldAccess;
    use ehci::{
        cap_regs::CapabilityRegisters,
        operational_regs::{OperationalRegisters, PortStatusCtl},
    };
    use futures_util::FutureExt;
    use hootux::alloc_interface::DmaAlloc;
    use hootux::fs::vfs::MajorNum;
    use volatile::{VolatilePtr, VolatileRef};

    pub(super) mod file;

    pub struct Ehci {
        capability_registers: &'static CapabilityRegisters,
        operational_registers: VolatilePtr<'static, OperationalRegisters>,
        address_bmp: u128,
        ports: Box<[VolatileRef<'static, PortStatusCtl>]>,
        async_list: Vec<alloc::sync::Arc<spin::Mutex<EndpointQueue>>>,

        memory: InaccessibleAddr<[u8]>,
        address: u32,
        layout: core::alloc::Layout,
        binding: hootux::fs::file::LockedFile<u8>,
        major_num: MajorNum,
        pci: alloc::sync::Arc<async_lock::Mutex<hootux::system::pci::DeviceControl>>,

        // workers
        pnp_watchdog_message: alloc::sync::Weak<hootux::task::util::WorkerWaiter>,
    }

    // SAFETY: Ehci is not Send because `operational_registers` contains `VolatilePtr` which is not Send
    // this field is may not be explicitly accessed by other types. Methods are required to operate on these fields.
    unsafe impl Send for Ehci {}
    unsafe impl Sync for Ehci {}

    struct InaccessibleAddr<T: ?Sized> {
        addr: NonNull<T>,
    }

    // SAFETY: Address is inaccessible without owning it
    unsafe impl<T: ?Sized> Sync for InaccessibleAddr<T> {}
    unsafe impl<T: ?Sized> Send for InaccessibleAddr<T> {}

    impl<T: ?Sized> InaccessibleAddr<T> {
        const fn new(addr: NonNull<T>) -> Self {
            Self { addr }
        }
        const fn get_addr(self) -> NonNull<T> {
            self.addr
        }
    }

    impl Ehci {
        /// This fn constructs a `Controller`
        /// This does not perform any initialisation.
        /// If this fn determines that the entire host controller cannot be accessed then it will return `None`.
        ///
        /// # Safety
        ///
        /// The caller must guarantee that `hci_pointer` outlives the returned `Controller` and that
        /// `hci_pointer` points to a valid Enhanced Host Controller interface.
        ///
        /// The caller should ensure that `hci_pointer` points to the entire region described by BAR0 in
        /// the PCI configuration region.
        pub unsafe fn new(
            hci_pointer: NonNull<[u8]>,
            phys_address: u32,
            layout: core::alloc::Layout,
            binding: hootux::fs::file::LockedFile<u8>,
            pci: alloc::sync::Arc<async_lock::Mutex<hootux::system::pci::DeviceControl>>,
        ) -> Option<Self> {
            let len = hci_pointer.len();
            let op_offset = unsafe { core::ptr::read_volatile(hci_pointer.cast::<u8>().as_ptr()) };
            if op_offset as usize + size_of::<OperationalRegisters>() > len {
                return None;
            }

            // SAFETY: The caller must guarantee that this is safe to deref. These registers are not volatile and can be safely dereferenced.
            let cap_regs = unsafe { hci_pointer.cast::<CapabilityRegisters>().as_ref() };
            // SAFETY: The caller must guarantee that this
            let op_regs = unsafe {
                VolatilePtr::new(NonNull::new(cap_regs.get_operational_registers()).unwrap())
            }; // Pointer was offset from`cap_regs` this cannot be Null (unless it wrapped around I guess)

            let port_count = cap_regs.struct_params.port_count() as usize;
            let last_port = OperationalRegisters::get_port(op_regs.as_raw_ptr(), port_count);

            // SAFETY: The unsafe pointer offsets in this block are safe because the resulting pointers are never dereferenced.
            // The are used to determine that the pointer is valid within `hci_pointer`
            {
                let head = hci_pointer.cast::<u8>().as_ptr();
                let tail = unsafe { head.add(len) };
                let t = unsafe { last_port.add(1).byte_sub(1).cast() };
                if !(head..tail).contains(&t) {
                    return None;
                }
            }

            let mut port_vec = Vec::with_capacity(port_count);
            for portnum in 0..port_count {
                let port_ptr = OperationalRegisters::get_port(op_regs.as_raw_ptr(), portnum);
                // SAFETY: Pointer is valid and exclusive, a major logic error has occured if `port_ptr` is NULL
                port_vec.push(unsafe { VolatileRef::new(NonNull::new_unchecked(port_ptr)) });
            }

            Some(Self {
                capability_registers: cap_regs,
                operational_registers: op_regs,
                address_bmp: 1,
                ports: port_vec.into_boxed_slice(),
                async_list: Vec::new(),
                memory: InaccessibleAddr::new(hci_pointer),
                address: phys_address,
                layout,
                binding,
                pci,
                major_num: MajorNum::new(),
                pnp_watchdog_message: alloc::sync::Weak::new(),
            })
        }

        /// Sets the configured flag to route ports to the EHCI, and clears the `CTRLDSSEGMENT` register to `0`
        async fn configure(&mut self) {
            let cap_params = self
                .capability_registers
                .capability_params
                .extended_capabilities();
            if cap_params != 0 {
                assert!(
                    cap_params >= 0x40,
                    "Faulty EHCI, has illegal extended capability parameters value {cap_params}"
                ); // todo we should handle this by returning error and leaking the binding
                let mut l = self.pci.lock_arc().await;
                let base = l.get_cfg_region_raw().cast::<u8>();
                // SAFETY: USB spec guarantees that the configuration-region + cap_parms contains the
                let legacy_sup_reg = unsafe { base.byte_add(cap_params as usize) }
                    .cast::<ehci::LegacySupportRegister>();
                // SAFETY: Guaranteed to point to the legacy support register
                // This is aliased but the other reference expects this to be volatile.
                unsafe {
                    ehci::LegacySupportRegister::set_os_semaphore(legacy_sup_reg);
                    ehci::LegacySupportRegister::wait_for_release(legacy_sup_reg);
                };
            }

            let cfg_flags = unsafe {
                // SAFETY: configure_flag is ConfigureFlag so this is safe.
                self.operational_registers.map(|p| {
                    p.byte_add(core::mem::offset_of!(OperationalRegisters, cfg_flags))
                        .cast::<ehci::operational_regs::ConfigureFlag>()
                })
            };
            cfg_flags.write(ehci::operational_regs::ConfigureFlag::RoutePortsToSelf);

            let segment = unsafe {
                // SAFETY: configure_flag is ConfigureFlag so this is safe.
                self.operational_registers.map(|p| {
                    p.byte_add(core::mem::offset_of!(OperationalRegisters, g4_seg_selector))
                        .cast::<u32>()
                })
            };
            segment.write(0); // always use mem32
        }

        /// Spawns the port change watchdog, which will handle initialising ports owned by the controller.
        ///
        /// This fn is `async` because it requires locking `this`, this can be run synchronously
        /// without blocking if the caller can guarantee that `this` is not locked.
        async fn start_port_watchdog(this: &alloc::sync::Arc<async_lock::Mutex<Self>>) {
            let wd = PnpWatchdog {
                controller: alloc::sync::Arc::downgrade(this),
                ports: core::mem::take(&mut this.lock().await.ports),
            };
            // todo: Can I make this a child or something in the future?
            hootux::task::run_task(wd.run().boxed());
        }

        /// Returns an address in the range 1..128
        fn alloc_address(&mut self) -> u8 {
            let bit = self.address_bmp.trailing_ones();
            self.address_bmp.set_bit(bit as usize, true);

            self.address_bmp;
            bit as u8
        }

        /// Frees the given address allocated by [Self::alloc_address]
        ///
        /// # Panics
        ///
        /// This fn will panic if `address == 0` or `address => 128` or if the address is already free
        fn free_address(&mut self, address: u8) {
            assert!(address < 128);
            assert_ne!(address, 0, "Cannot free address 0");
            assert!(
                self.address_bmp.bit(address as usize),
                "Attempted double free"
            );
            self.address_bmp.set_bit(address as usize, false)
        }

        pub fn handle_interrupt(&mut self) {
            use ehci::operational_regs::UsbStatus;
            let sts_reg = self.operational_registers.usb_status();
            let int_status = sts_reg.read();

            for i in int_status.int_only().iter() {
                match i {
                    UsbStatus::USB_INT => {
                        sts_reg.write(UsbStatus::USB_INT);
                        todo!()
                    }
                    UsbStatus::USB_ERROR_INT => {
                        sts_reg.write(UsbStatus::USB_ERROR_INT);
                        todo!()
                    }
                    UsbStatus::PORT_CHANGE_DETECT => {
                        sts_reg.write(UsbStatus::PORT_CHANGE_DETECT);
                        let Some(waiter) = self.pnp_watchdog_message.upgrade() else {
                            continue;
                        }; // What? Are we shutting down? Starting Up?
                        waiter.wake()
                    }
                    UsbStatus::FRAME_LIST_ROLLOVER => {
                        sts_reg.write(UsbStatus::FRAME_LIST_ROLLOVER);
                        todo!()
                    }
                    UsbStatus::HOST_SYSTEM_ERROR => {
                        sts_reg.write(UsbStatus::HOST_SYSTEM_ERROR);
                        todo!()
                    }
                    UsbStatus::INTERRUPT_ON_ASYNC_ADVANCE => {
                        sts_reg.write(UsbStatus::INTERRUPT_ON_ASYNC_ADVANCE);
                        todo!()
                    }
                    _ => unreachable!(), // All remaining bits are masked out
                }
            }
        }

        fn insert_into_async(&mut self, queue: alloc::sync::Arc<spin::Mutex<EndpointQueue>>) {
            let mut l = queue.lock();
            let addr: u32 = hootux::mem::mem_map::translate_ptr(self.async_list.first().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            l.head.set_next_queue_head(addr);

            let addr: u32 = hootux::mem::mem_map::translate_ptr(&*l.head)
                .unwrap()
                .try_into()
                .unwrap();

            self.async_list
                .last()
                .unwrap()
                .lock()
                .head
                .set_next_queue_head(addr);

            drop(l);
            self.async_list.push(queue)
        }

        /// Fetches the default table, which is configured for the control-pipe of the default address.
        /// This should only be used to allocate a non-default address to the device.
        fn get_default_table(&self) -> alloc::sync::Arc<spin::Mutex<EndpointQueue>> {
            self.async_list[0].clone()
        }
    }

    impl Drop for Ehci {
        fn drop(&mut self) {
            let addr = core::mem::replace(
                &mut self.memory,
                // SAFETY: We just need to take the address, it doesnt matter what we replace it with because it will never be accessed again.
                InaccessibleAddr::new(unsafe {
                    NonNull::new(core::slice::from_raw_parts_mut(
                        core::ptr::dangling_mut(),
                        0,
                    ))
                    .unwrap()
                }),
            )
            .get_addr();
            // SAFETY: This is safe because we only free memory
            let alloc = unsafe { hootux::alloc_interface::MmioAlloc::new(self.address as usize) };
            // SAFETY: self.capability_registers and self.operational_registers point to this.
            // But because they are &/VolatilePtr they are not dropped.
            unsafe { alloc.deallocate(addr.cast(), self.layout) };
        }
    }

    fn init_default_device(queue_head: &mut EndpointQueue) {
        const DEFAULT_CONFIG_TARGET: Target = Target {
            dev: DeviceAddress::Default,
            endpoint: Endpoint::new(0).unwrap(),
        };
        if queue_head.get_target() != DEFAULT_CONFIG_TARGET {
            panic!("Attempted to assign address to {:?}", queue_head.target)
        }
    }

    /// The EndpointQueue maintains the state of queued operations for asynchronous jobs.
    ///
    /// The EndpointQueue operates using [TransactionString]'s, which each describes a queued operation.
    #[derive(Derivative)]
    #[derivative(Ord, PartialEq, PartialOrd, Eq)]
    struct EndpointQueue {
        // For some reason the PID is in the QTD, not the QueueHead so we need to keep the PID
        // I'm sure I'll figure out why soon enough
        #[derivative(PartialEq = "ignore")]
        #[derivative(Ord = "ignore")]
        #[derivative(PartialOrd = "ignore")]
        pid: crate::PidCode,
        target: crate::Target,
        #[derivative(PartialEq = "ignore")]
        #[derivative(Ord = "ignore")]
        #[derivative(PartialOrd = "ignore")]
        packet_size: u32,
        #[derivative(PartialEq = "ignore")]
        #[derivative(Ord = "ignore")]
        #[derivative(PartialOrd = "ignore")]
        head: Box<QueueHead, DmaAlloc>,
        #[derivative(PartialEq = "ignore")]
        #[derivative(Ord = "ignore")]
        #[derivative(PartialOrd = "ignore")]
        work: Vec<TransactionString>,
        // Option because this isn't required when transactions are running.
        // This can be used as a cached descriptor
        #[derivative(PartialEq = "ignore")]
        #[derivative(Ord = "ignore")]
        #[derivative(PartialOrd = "ignore")]
        terminator: Option<Box<QueueElementTransferDescriptor, DmaAlloc>>,
    }

    impl EndpointQueue {
        fn new(target: super::Target, pid: super::PidCode, packet_size: u32) -> Self {
            let mut this = Self {
                pid,
                target,
                packet_size,
                head: Box::new_in(
                    QueueHead::new(),
                    DmaAlloc::new(hootux::mem::MemRegion::Mem32, 32),
                ),
                work: Vec::new(),
                terminator: Some(Box::new_in(
                    QueueElementTransferDescriptor::new(),
                    DmaAlloc::new(hootux::mem::MemRegion::Mem32, 32),
                )),
            };

            // SAFETY: The address is a valid terminated table.
            unsafe {
                this.head.set_current_transaction(
                    hootux::mem::mem_map::translate(
                        this.terminator.as_ref().unwrap() as *const _ as usize
                    )
                    .expect("What? Static isn't mapped?") as u32,
                )
            };

            this.head
                .set_target(target.try_into().expect("Invalid target"));
            this
        }

        fn head_of_list() -> Self {
            let mut this = Self::new(
                crate::Target {
                    dev: crate::DeviceAddress::Default,
                    endpoint: crate::Endpoint::new(0).unwrap(),
                },
                crate::PidCode::Control,
                8,
            );
            this.head.set_head_of_list();
            this
        }

        fn new_string(
            &mut self,
            payload: Box<dyn hootux::mem::dma::DmaTarget>,
            int_mode: StringInterruptConfiguration,
        ) -> alloc::sync::Arc<hootux::task::util::WorkerWaiter> {
            let st = TransactionString::new(payload, self.packet_size, int_mode);
            let fut = st.get_future();
            let Some(last) = self.work.last_mut() else {
                return fut;
            };
            // SAFETY: Self ensures that the string is either run to completion or safely removed.
            unsafe { last.append_string(&st) }

            fut
        }

        const fn get_target(&self) -> super::Target {
            self.target
        }

        fn append_cmd_string(
            &mut self,
            string: TransactionString,
        ) -> alloc::sync::Arc<hootux::task::util::WorkerWaiter> {
            let fut = string.get_future();
            if let Some(last) = self.work.last_mut() {
                // SAFETY: `String` is guaranteed to either be completed or safely aborted.
                unsafe { last.append_string(&string) };
            }
            self.work.push(string);
            fut
        }
    }

    struct PnpWatchdog {
        controller: alloc::sync::Weak<async_lock::Mutex<Ehci>>,
        ports: Box<[VolatileRef<'static, PortStatusCtl>]>, // ports are owned by self.controller, we must upgrade the controller first.
    }

    // SAFETY: This is safe because we never call into_inner from the mutex
    unsafe impl Send for PnpWatchdog {}

    impl PnpWatchdog {
        async fn run(self) -> hootux::task::TaskResult {
            match self.run_inner().await {
                Ok(()) => hootux::task::TaskResult::ExitedNormally,
                Err(()) => hootux::task::TaskResult::StoppedExternally,
            }
        }

        async fn run_inner(mut self) -> Result<(), ()> {
            #[derive(Copy, Clone, Debug)]
            enum Work {
                Removed,
                Added,
            }
            for i in &mut self.ports {
                i.as_mut_ptr().update(|mut p| {
                    p.set_port_power(true);
                    p.wake_on_connect(true);
                    p.wake_on_disconnect(true);
                    p.wake_on_overcurrent(true);
                    p
                });
                assert!(i.as_ptr().read().port_power());
            }
            hootux::task::util::sleep(100).await;
            let work = alloc::sync::Arc::new(hootux::task::util::WorkerWaiter::new());
            let mut controller = self.controller.upgrade().ok_or(())?.lock_arc().await;
            controller.pnp_watchdog_message = alloc::sync::Arc::downgrade(&work);
            drop(controller);
            // This will add startup work to init ports.
            // If the port status change bit is set then this will be overwritten by the normal runtime loop
            let mut work_list: [Option<Work>; 64] = core::array::from_fn(|portnum| {
                if self.ports.get(portnum)?.as_ptr().read().connected() {
                    Some(Work::Added)
                } else {
                    None
                }
            });

            loop {
                // This acts as a bit like a hardware mutex.
                // It ensures that the controller is still there before acting and that it will not be dropped while we are working.
                let controller = self.controller.upgrade().ok_or(())?;
                for (i, port) in self.ports.iter_mut().enumerate() {
                    let r = port.as_ptr().read();
                    if r.connect_status_change() {
                        port.as_mut_ptr()
                            .write(PortStatusCtl(PortStatusCtl::ACK_STATUS_CHANGE));
                        match r.connected() {
                            true => work_list[i] = Some(Work::Added),
                            false => work_list[i] = Some(Work::Removed),
                        }
                        log::trace!("Work for port {i} queued: {:?}", work_list[i].unwrap());
                    }
                }

                'work_loop: for (i, w) in work_list
                    .iter_mut()
                    .enumerate()
                    .map(|(i, w)| (i, w.as_ref()))
                {
                    match w {
                        None => continue 'work_loop,
                        Some(Work::Added) => {
                            let port = &mut self.ports[i];

                            port.as_mut_ptr().update(|mut s| {
                                s.reset(true);
                                s
                            });
                            hootux::task::util::sleep(50).await;
                            port.as_mut_ptr().update(|mut s| {
                                s.reset(false);
                                s
                            });

                            assert!(
                                port.as_ptr().read().enabled(),
                                "Port not enabled {:?}",
                                port.as_ptr().read()
                            );
                            let mut ctl_lock = controller.lock().await;
                            let b = ctl_lock.get_default_table();
                            let mut default_table = b.lock();
                            let new_address = ctl_lock.alloc_address();
                            drop(ctl_lock);

                            let mut command = hootux::mem::dma::DmaGuard::new(Vec::from(
                                usb_cfg::CtlTransfer::set_address(new_address).to_bytes(),
                            ));
                            let mut ts = TransactionString::empty();
                            unsafe { ts.append_qtd(&mut command, PidCode::Control) };
                            default_table.append_cmd_string(ts).wait().await;

                            let eq = EndpointQueue::new(
                                Target {
                                    dev: DeviceAddress::Address(new_address.try_into().unwrap()),
                                    endpoint: Endpoint::new(0).unwrap(),
                                },
                                PidCode::Control,
                                64,
                            );
                            let mut ctl_lock = controller.lock().await;
                            ctl_lock.insert_into_async(alloc::sync::Arc::new(spin::Mutex::new(eq)));

                            log::info!("Port {i} initialised as {new_address}");
                        }
                        Some(Work::Removed) => todo!(), // free all resources attached to the port
                    }
                }
            }
        }
    }

    /// A `TransactionString` describes a series of expected transactions.
    /// Due to the limited buffer size of a single [QueueElementTransferDescriptor] a large number
    /// of them may be required for a single expected operation.
    struct TransactionString {
        // This can be changed into `Box<[QTD],DmaAlloc>` which may be more optimal
        // current form is more flexible
        // May never have 0 elements
        str: Box<[Box<QueueElementTransferDescriptor, DmaAlloc>]>,
        waiter: alloc::sync::Arc<hootux::task::util::WorkerWaiter>,
    }

    impl TransactionString {
        fn new(
            mut payload: Box<dyn hootux::mem::dma::DmaTarget>,
            transaction_len: u32,
            interrupt: StringInterruptConfiguration,
        ) -> Self {
            let len = payload.len();
            let offset_into_initial = payload.data_ptr().cast::<u8>() as usize & PAGE_SIZE - 1;

            let mut prd = payload
                .prd()
                .flat_map(|r| {
                    let start_frame = r.addr & !(PAGE_SIZE - 1) as u64;
                    (start_frame..r.addr + r.size as u64).step_by(PAGE_SIZE)
                })
                .peekable();

            let mut string: Vec<Box<QueueElementTransferDescriptor, DmaAlloc>> = Vec::new();

            const BOUNDS_ERR: &str = "Bounds checking error in TransactionString::new()";

            // cursor indicates how far into the buffer we are.
            // This method is used because it indicates the offset into the current page, when qtd's have overlapping pages.
            let tgt_len = len + offset_into_initial;
            let mut cursor = offset_into_initial;
            while cursor < tgt_len {
                // number of transactions in this qtd
                let packets = suffix::bin!(20Ki).min(tgt_len - cursor) / transaction_len as usize;
                let qtd_len_bytes = packets * transaction_len as usize;
                let qtd_pages = qtd_len_bytes.div_ceil(PAGE_SIZE);
                let peek_last = qtd_pages == 5 && qtd_len_bytes / PAGE_SIZE == 4; // if qtd_pages is rounded up

                let mut qtd = QueueElementTransferDescriptor::new();

                for i in 0..qtd_pages {
                    if i == 5 && peek_last {
                        let addr = *prd.peek().expect(BOUNDS_ERR);
                        qtd.set_buffer(i, addr);

                        break;
                    }
                    qtd.set_buffer(i, prd.next().expect(BOUNDS_ERR))
                }
                cursor += qtd_len_bytes;

                let mut b = Box::<QueueElementTransferDescriptor, DmaAlloc>::new_uninit_in(
                    DmaAlloc::new(hootux::mem::MemRegion::Mem32, 32),
                );
                // SAFETY: write is safe, pointer is coerced from a reference.
                unsafe { b.as_mut_ptr().write_volatile(qtd) };
                // SAFETY: `b` is initialised above.
                let b = unsafe { b.assume_init() };
                if let Some(last) = string.last_mut() {
                    // SAFETY: This fetches the current address of the table. We guarantee the qtd is never moved from here.
                    unsafe {
                        last.set_next(Some(
                            hootux::mem::mem_map::translate_ptr(&*b)
                                .unwrap()
                                .try_into()
                                .unwrap(),
                        ))
                    };
                }
                string.push(b);
            }

            match interrupt {
                StringInterruptConfiguration::Never => {}
                StringInterruptConfiguration::End => {
                    let last = string.last_mut().unwrap();
                    last.set_int_on_complete();
                }
                StringInterruptConfiguration::Always => {
                    for i in &mut string {
                        i.set_int_on_complete();
                    }
                }
            };

            Self {
                str: string.into_boxed_slice(),
                waiter: alloc::sync::Arc::new(hootux::task::util::WorkerWaiter::new()),
            }
        }

        /// When this string has completed `other` will be run.
        ///
        /// When this is called `other` will be loaded into the final `QTD.next` field and all
        /// `QTD.alternate` fields
        ///
        /// # Safety
        ///
        /// The caller must guarantee that when `self` may be executed that `other` will not be
        /// dropped until it's completed execution or the controller is stopped.
        unsafe fn append_string(&mut self, other: &Self) {
            // unwrap: other.str[0] was not mapped into memory?
            let next_addr: u32 = hootux::mem::mem_map::translate_ptr(&*other.str[0])
                .unwrap()
                .try_into()
                .expect("QueueTransportDescriptor mapped into Mem64 memory");
            let tail = self.str.last_mut().unwrap();
            // SAFETY: Next addr is guaranteed to point to point to a valid QTD
            // The caller must guarantee that `other` remains valid
            unsafe {
                tail.set_next(Some(next_addr));
                tail.set_alternate(Some(next_addr));
            };

            for i in &mut self.str {
                // SAFETY: Next addr is guaranteed to point to point to a valid QTD
                // The caller must guarantee that `other` remains valid
                unsafe { i.set_alternate(Some(next_addr)) };
            }
        }

        /// Aborts execution of this transaction string by clearing all active bits in the QTDs.
        /// This causes the controller to iterate over them until either the last QTD is reached or
        /// a QTD in another `TransactionString` is reached.
        pub fn abort(&mut self) {
            // Do it in reverse order, to prevent race conditions.
            for i in self.str.iter_mut().rev() {
                i.set_active(false);
            }
        }

        /// Determines if `addr` points to one of the QTDs owned by `self`.
        fn addr_is_self(&self, addr: u32) -> bool {
            for i in &self.str {
                // unwrap: All QTDs must be in Dma32 memory
                let i_t: &QueueElementTransferDescriptor = &**i; // just ensures the type is correct
                let i_addr: u32 = hootux::mem::mem_map::translate_ptr(i_t)
                    .unwrap()
                    .try_into()
                    .unwrap();
                if i_addr == addr {
                    return true;
                }
            }
            false
        }

        fn get_future(&self) -> alloc::sync::Arc<hootux::task::util::WorkerWaiter> {
            self.waiter.clone()
        }
    }

    /// Methods for constructing `TransactionString`s for the default command pipe.
    impl TransactionString {
        fn empty() -> Self {
            Self {
                str: Box::new([]),
                waiter: alloc::sync::Arc::new(hootux::task::util::WorkerWaiter::new()),
            }
        }

        /// Constructs and appends a single qtd to `self`.
        ///
        /// # Panics
        ///
        /// `payload.len()` must not exceed the QTD buffer size (20KiB).
        ///
        /// # Safety
        ///
        /// The caller must ensure that `payload` is not dropped before or while the controller
        /// performs DMA to `payload`.
        unsafe fn append_qtd<'a, 'b>(
            &'a mut self,
            payload: &'b mut dyn hootux::mem::dma::DmaTarget,
            pid: super::PidCode,
        ) -> &'a mut Self {
            let str = core::mem::take(&mut self.str);
            let mut c = str.into_vec();
            let mut qtd = QueueElementTransferDescriptor::new();
            assert!(payload.len() <= 5 * PAGE_SIZE);

            let mut page = 0;
            for i in payload.prd() {
                let aligned = i.addr & !(PAGE_SIZE - 1) as u64;
                let len = i.size + (i.addr as usize) & (PAGE_SIZE - 1);
                assert!(len <= 5 * PAGE_SIZE);
                qtd.set_buffer(i.addr as usize, aligned);
                for _ in 0..len / PAGE_SIZE {
                    qtd.set_buffer(page, aligned + (page * PAGE_SIZE) as u64);
                    page += 1;
                }
            }

            qtd.set_pid(pid);
            qtd.set_active(true);
            let mut b = Box::<QueueElementTransferDescriptor, _>::new_uninit_in(DmaAlloc::new(
                hootux::mem::MemRegion::Mem32,
                32,
            ));
            // SAFETY: MaybeUninit ensures this is aligned.
            // assume_init: Is safe because we initialise `b`
            unsafe {
                b.as_mut_ptr().write_volatile(qtd);
                c.push(b.assume_init());
            }
            self.str = c.into_boxed_slice();
            self
        }
    }

    #[derive(Debug, Eq, PartialEq, Copy, Clone)]
    pub enum StringInterruptConfiguration {
        /// Indicates the controller should not raise an interrupt when executing a [TransactionString]
        Never,
        /// Indicates the controller should raise an interrupt when the transaction descriptor has been completed.
        /// This should be use when the data length is definitively known.
        /// If less data is returned than expected the future may not be woken.
        End,
        /// Indicates the controller should raise an interrupt when any transaction descriptor is completed.
        /// When only one transaction descriptor is present this acts the same as [Self::End]
        Always,
    }
}

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
                    // SAFETY: This is safe because the the address and layout is guaranteed to be correct.
                    // Both values are fetched directly from the BAR
                    let ehci = unsafe {
                        ehci::file::EhciFileContainer::new(
                            bind,
                            bar_info.addr().try_into().unwrap(),
                            bar_info.layout(),
                        )
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
