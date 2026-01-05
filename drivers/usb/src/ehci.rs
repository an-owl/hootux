use crate::{DeviceAddress, Endpoint, PAGE_SIZE, PidCode, Target};
use alloc::boxed::Box;
use alloc::vec::Vec;
use bitfield::{Bit, BitMut};
use core::alloc::Allocator;
use core::cmp::PartialEq;
use core::pin::Pin;
use core::ptr::NonNull;
use core::task::{Context, Poll};
use derivative::Derivative;
use ehci::frame_lists::{PeriodicFrameList, QueueElementTransferDescriptor, QueueHead};
use ehci::operational_regs::{IntEnable, OperationalRegistersVolatileFieldAccess};
use ehci::{
    cap_regs::CapabilityRegisters,
    operational_regs::{OperationalRegisters, PortStatusCtl},
};
use futures_util::FutureExt;
use hootux::alloc_interface::DmaAlloc;
use hootux::fs::vfs::MajorNum;
use hootux::mem::dma::DmaBuff;
use hootux::task::util::WorkerWaiter;
use volatile::{VolatilePtr, VolatileRef};

pub(super) mod device;
pub(super) mod file;

pub struct Ehci {
    capability_registers: &'static CapabilityRegisters,
    operational_registers: VolatilePtr<'static, OperationalRegisters>,
    address_bmp: u128,
    ports: Box<[VolatileRef<'static, PortStatusCtl>]>,
    async_list: Vec<alloc::sync::Arc<EndpointQueue>>,
    // This is Option because the frame list is allocated by Self::configure() so the pointer can be set at the same time
    // At runtime callers can assume this is Some
    periodic_frame_list: Option<Box<PeriodicFrameList, DmaAlloc>>,

    /// Periodic endpoints, this list contains the reverse order of execution of the periodic queues.
    /// This must be sorted by the endpoints "period".
    periodic_queue_heads: Vec<PeriodicEndpointQueue>,

    memory: InaccessibleAddr<[u8]>,
    address: u32,
    layout: core::alloc::Layout,
    _binding: hootux::fs::file::LockedFile<u8>,
    major_num: MajorNum,
    pci: alloc::sync::Arc<async_lock::Mutex<hootux::system::pci::DeviceControl>>,

    // workers
    pnp_watchdog_message: alloc::sync::Weak<WorkerWaiter>,
    interrupt_worker: alloc::sync::Weak<InterruptWorker>,
    int_handler: Option<alloc::sync::Arc<IntHandler>>,

    // Maintained state. Key is the port number. Values above 15 are not allowed.
    port_files: alloc::collections::BTreeMap<u8, alloc::sync::Arc<device::UsbDeviceAccessor>>,

    async_doorbell_mutex: async_lock::Semaphore,
    doorbell_waker: alloc::sync::Arc<futures_util::task::AtomicWaker>,
}

// SAFETY: Ehci is not Send because `operational_registers` contains `VolatilePtr` which is not Send
// this field is may not be explicitly accessed by other types. Methods are required to operate on these fields.
unsafe impl Send for Ehci {}

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
            periodic_queue_heads: Vec::new(),
            periodic_frame_list: None,
            memory: InaccessibleAddr::new(hci_pointer),
            address: phys_address,
            layout,
            _binding: binding,
            pci,
            major_num: MajorNum::new(),
            pnp_watchdog_message: alloc::sync::Weak::new(),
            interrupt_worker: alloc::sync::Weak::new(),
            int_handler: None,
            port_files: alloc::collections::BTreeMap::new(),
            async_doorbell_mutex: async_lock::Semaphore::new(1),
            doorbell_waker: alloc::sync::Arc::new(futures_util::task::AtomicWaker::new()),
        })
    }

    fn setup_periodic_list(&mut self) {
        let periodic_list = Box::new_in(
            PeriodicFrameList::new(),
            DmaAlloc::new(hootux::mem::MemRegion::Mem32, PAGE_SIZE),
        );
        let list_ref: &PeriodicFrameList = &*periodic_list;
        // DmaAlloc(Mem32) will guarantee that this always returns Some(<u32::MAX)
        self.operational_registers
            .frame_list_addr()
            .write(hootux::mem::mem_map::translate_ptr(list_ref).unwrap() as u32);
        self.periodic_frame_list = Some(periodic_list)
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
            let legacy_sup_reg =
                unsafe { base.byte_add(cap_params as usize) }.cast::<ehci::LegacySupportRegister>();
            // SAFETY: Guaranteed to point to the legacy support register
            // This is aliased but the other reference expects this to be volatile.
            unsafe {
                ehci::LegacySupportRegister::set_os_semaphore(legacy_sup_reg);
                ehci::LegacySupportRegister::wait_for_release(legacy_sup_reg);
            };
        }

        self.controller_enable(false);

        hootux::task::util::sleep(2).await;
        if !self.is_halted() {
            let timeout: hootux::time::AbsoluteTime = hootux::time::Duration::millis(2).into();
            log::trace!("EHCI not disabled after deadline");
            while !self.is_halted() {
                core::hint::spin_loop();
                if timeout.is_future() {
                    panic!("EHCI took wayyyy too long to halt")
                }
            }
        }

        self.operational_registers.g4_seg_selector().write(0); // always use mem32

        self.execute_periodic(false);
        self.execute_async(false);
        self.init_head_table();

        self.operational_registers.int_enable().update(|mut sts| {
            sts.set(
                IntEnable::FRAME_LIST_ROLLOVER | IntEnable::INTERRUPT_ON_ASYNC_ADVANCE,
                false,
            );
            sts.set(
                IntEnable::HOST_SYSTEM_ERROR
                    | IntEnable::PORT_CHANGE_DETECT
                    | IntEnable::USB_ERROR_INT
                    | IntEnable::USB_INT,
                true,
            );
            sts
        });

        self.setup_periodic_list();
        self.controller_enable(true);
        let cfg_flags = unsafe {
            // SAFETY: configure_flag is ConfigureFlag so this is safe.
            self.operational_registers.map(|p| {
                p.byte_add(core::mem::offset_of!(OperationalRegisters, cfg_flags))
                    .cast::<ehci::operational_regs::ConfigureFlag>()
            })
        };
        cfg_flags.write(ehci::operational_regs::ConfigureFlag::RoutePortsToSelf);
        self.execute_periodic(true);
        self.execute_async(true);
    }

    /// Spawns the port change watchdog, which will handle initialising ports owned by the controller.
    ///
    /// This fn is `async` because it requires locking `this`, this can be run synchronously
    /// without blocking if the caller can guarantee that `this` is not locked.
    async fn start_port_watchdog(this: &alloc::sync::Arc<async_lock::Mutex<Self>>) {
        let mut l = this.lock().await;
        let wd = PnpWatchdog {
            controller: alloc::sync::Arc::downgrade(this),
            ports: core::mem::take(&mut l.ports),
            work: alloc::sync::Arc::new(WorkerWaiter::new()),
            major_num: l.major_num,
        };
        l.pnp_watchdog_message = alloc::sync::Arc::downgrade(&wd.work);
        // todo: Can I make this a child or something in the future?
        hootux::task::run_task(wd.run().boxed());
    }

    async fn start_int_worker(this: &alloc::sync::Arc<async_lock::Mutex<Self>>) {
        let mut l = this.lock_arc().await;
        let None = l.interrupt_worker.upgrade() else {
            panic!("Attempted to start interrupt worker twice")
        };
        let weak = alloc::sync::Arc::downgrade(this);
        let iw = alloc::sync::Arc::new(InterruptWorker {
            queue: hootux::task::util::MessageQueue::new(4),
            parent: weak,
        });

        l.interrupt_worker = alloc::sync::Arc::downgrade(&iw);
        hootux::task::run_task(iw.run().boxed());
    }

    async fn get_int_handler(
        this: alloc::sync::Arc<async_lock::Mutex<Self>>,
    ) -> alloc::sync::Arc<IntHandler> {
        Ehci::start_port_watchdog(&this).await;
        Ehci::start_int_worker(&this).await;

        let mut l = this.lock_arc().await;
        match &mut l.int_handler {
            Some(a) => a.clone(),
            None => {
                let ih = IntHandler {
                    parent: alloc::sync::Arc::downgrade(&this),
                    // SAFETY: All code accessing this field must ensure that `self.parent` is upgraded first.
                    status_register: unsafe {
                        VolatilePtr::new(l.operational_registers.usb_status().as_raw_ptr())
                    },
                    interrupt_worker: l.interrupt_worker.upgrade().unwrap().queue.sender(),
                    pnp_watchdog: l.pnp_watchdog_message.clone(),
                    poll_period: core::sync::atomic::AtomicU64::new(0),
                    polling: core::sync::atomic::AtomicBool::new(false),
                    async_doorbell: alloc::sync::Arc::downgrade(&l.doorbell_waker),
                };
                let ih = alloc::sync::Arc::new(ih);
                l.int_handler = Some(ih.clone());
                ih
            }
        }
    }

    /// Enables/Disables polling, or changes the polling rate.
    /// Setting `msec` to `None` will cause polling to stop.
    fn start_polling(&self, msec: Option<core::num::NonZeroU64>) {
        let t = self.int_handler.as_ref().unwrap();
        let time = msec.map(|t| t.get()).unwrap_or(0);
        t.poll_period
            .store(time, core::sync::atomic::Ordering::SeqCst);
        // Checks if polling is already active.
        if !t.polling.load(core::sync::atomic::Ordering::SeqCst) {
            hootux::task::run_task(Box::pin(t.clone().poll()))
        }
    }

    fn is_halted(&self) -> bool {
        self.operational_registers
            .usb_status()
            .read()
            .contains(ehci::operational_regs::UsbStatus::CONTROLLER_HALTED)
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
    fn free_address(&mut self, address: DeviceAddress) {
        assert_ne!(address, DeviceAddress::Default, "Cannot free address 0");
        let addr: u8 = address.into();
        assert!(self.address_bmp.bit(addr as usize), "Attempted double free");
        self.address_bmp.set_bit(addr as usize, false)
    }

    fn init_head_table(&mut self) {
        let head = EndpointQueue::head_of_list();
        debug_assert!(
            self.async_list.first().is_none(),
            "async list already started"
        );
        self.async_list.push(alloc::sync::Arc::new(head));
        let op_regs = self.operational_registers;
        let async_list = volatile::map_field!(op_regs.async_list_addr);

        let start = self.async_list.first().unwrap();

        async_list.write(start.head_addr());
    }

    /// Inserts a queue head into the asynchronous queue head list.
    ///
    /// # Deadlocks
    ///
    /// This requires locking the last entry in the list, the caller must ensure that it is free.
    fn insert_into_async(&mut self, queue: alloc::sync::Arc<EndpointQueue>) {
        queue.set_next_endpoint_queue(self.async_list.first().unwrap());
        let last = self.async_list.last().unwrap();
        last.set_next_endpoint_queue(&queue);

        self.async_list.push(queue)
    }

    /// Fetches the default table, which is configured for the control-pipe of the default address.
    /// This should only be used to allocate a non-default address to the device.
    fn get_default_table(&self) -> alloc::sync::Arc<EndpointQueue> {
        self.async_list[0].clone()
    }

    fn execute_async(&mut self, state: bool) {
        let regs = self.operational_registers;
        let cfg = volatile::map_field!(regs.usb_command);
        cfg.update(|mut cmd| {
            cmd.set_async_schedule_enable(state);
            cmd
        })
    }

    /// Sets the periodic schedule execution. The periodic schedule execution will not stop immediately,
    /// it will only stop on a periodic table entry boundary. Meaning it will only stop on a uframe*8 boundary.
    ///
    /// [Self::periodic_schedule_state] will return the current state of this bit.
    fn execute_periodic(&mut self, state: bool) {
        let regs = self.operational_registers;
        let cfg = volatile::map_field!(regs.usb_command);
        cfg.update(|mut cmd| {
            cmd.set_periodic_schedule_enable(state);
            cmd
        })
    }

    /// Returns whether the current state of the "periodic schedule enable" bit.
    fn periodic_schedule_state(&self) -> bool {
        let regs = self.operational_registers;
        let cfg = volatile::map_field!(regs.usb_command);
        cfg.read().periodic_schedule_enable()
    }

    fn controller_enable(&mut self, state: bool) {
        let regs = self.operational_registers;
        let cfg = volatile::map_field!(regs.usb_command);
        cfg.update(|mut cmd| {
            cmd.set_enable(state);
            cmd
        })
    }
    // You know the music, it's time to dance.
    async fn drop_endpoints<'a, T: Iterator<Item = &'a EndpointQueue>>(&'a mut self, endpoints: T) {
        let sem = self.async_doorbell_mutex.acquire().await;
        for i in endpoints {
            let Some(t) = self.async_list.iter().position(|e| {
                let list_ptr: &EndpointQueue = &*e;
                core::ptr::eq(list_ptr, i)
            }) else {
                panic!(
                    "Attempted to free EndpointQueue that was not found in this controller {:?}",
                    self.major_num
                )
            };
            i.waiting_for_drop
                .store(true, core::sync::atomic::Ordering::Release);

            // locate previous and next EndpointQueues.
            // Note that this is complicated because wee need to seek to the next not-removed queue heads
            let prev = self.async_list[..t]
                .iter()
                .rev()
                .filter(|e| {
                    !e.waiting_for_drop
                        .load(core::sync::atomic::Ordering::Relaxed)
                })
                .next()
                .expect("This should've selected head [0]");
            let next = self.async_list[t..]
                .iter()
                .filter(|e| {
                    !e.waiting_for_drop
                        .load(core::sync::atomic::Ordering::Relaxed)
                })
                .next()
                .unwrap_or(&self.async_list[0]);

            // Insert the next's address into the prev's `next` field
            // Each lock should be dropped asap
            let l_next = next.inner.lock();
            let next_addr = hootux::mem::mem_map::translate_ptr(&*l_next.head)
                .unwrap()
                .try_into()
                .unwrap();
            drop(l_next);
            let mut l_prev = prev.inner.lock();
            l_prev.head.set_next_queue_head(next_addr);
            drop(l_prev);
        }
        AsyncDoorbell::new(self, sem).await;
        self.async_list.retain(|e| {
            !e.waiting_for_drop
                .load(core::sync::atomic::Ordering::Relaxed)
        });
    }

    /// This will disable the periodic frame list, reconstruct the list then re-enable the list.
    async fn rebuild_periodic_list(&mut self) -> Result<(), ()> {
        self.execute_periodic(false);
        if self.periodic_queue_heads.len() == 0 {
            // No endpoints, just stop.
            return Ok(());
        }

        // This *should* indicate 5ms of retires (which is intended), but this is not bound to an exit time.
        // At the time of writing this should wait about 17ms before failing.
        const RETRIES: usize = 5;
        for i in 0..=RETRIES {
            match i {
                RETRIES => {
                    log::error!(
                        "EHCI {}: periodic list failed to stop ",
                        self.major_num.get_raw()
                    );
                    return Err(());
                }
                n => {
                    if !self.periodic_schedule_state() {
                        if n > 2 {
                            log::warn!(
                                "EHCI {}: periodic schedule took longer than expected to stop",
                                self.major_num.get_raw()
                            );
                        }
                        break;
                    }
                    hootux::task::util::sleep(1).await
                }
            }
        }

        // SAFETY: Periodic schedule has been disabled.
        unsafe { self.link_periodic_list() };

        let Some(ref mut periodic) = self.periodic_frame_list else {
            unreachable!()
        };
        for (i, ptr) in periodic.iter().enumerate() {
            let long_period = (i >> 3) as u32;

            let Some(head) = self.periodic_queue_heads.iter().rfind(|&e| {
                // If `long_period` is a multiple of the `e.rate/8` then we place it as the first entry in the list.
                long_period.checked_rem(e.rate / 8).unwrap_or(0) == 0
            }) else {
                continue;
            };

            let head_addr = head.endpoint.head_addr();

            ptr.set_address(head_addr);
            ptr.set_type(ehci::frame_lists::FrameListLinkType::QueueHead);
        }

        // Dont wait for it, it'll start when its ready.
        self.execute_periodic(true);
        Ok(())
    }

    /// Links all periodic queues in order to be run.
    ///
    ///  # Safety
    ///
    /// The caller must ensure that the periodic frame list is not running.
    unsafe fn link_periodic_list(&mut self) {
        for i in (0..self.periodic_queue_heads.len()).rev() {
            let this = &self.async_list[i];
            let Some(next_index) = i.checked_sub(1) else {
                this.terminate_horizontal();
                return;
            };
            let next = &self.async_list[next_index];
            this.set_next_endpoint_queue(&next);
        }
    }

    /// Inserts `queues` into the periodic schedule and re-configures the periodic frame list.
    ///
    /// If this fn returns `Err(())` the controller failed to stop
    pub async fn insert_into_periodic(
        &mut self,
        queues: impl Iterator<Item = PeriodicEndpointQueue>,
    ) -> Result<(), ()> {
        for queue in queues {
            queue.set_bitmap();
            let (Ok(entry) | Err(entry)) = self
                .periodic_queue_heads
                .binary_search_by(|f| f.rate.cmp(&queue.rate));
            self.periodic_queue_heads.insert(entry, queue);
        }

        self.rebuild_periodic_list().await
    }

    /// Drops periodic endpoints from the execution list.
    ///
    /// When this fn fn returns `Err(())` the periodic list failed to stop. Endpoints
    pub async fn drop_periodic_endpoints(
        &mut self,
        endpoints: impl Iterator<Item = &PeriodicEndpointQueue>,
    ) -> Result<(), ()> {
        let mut dirty = false;
        for endpoint in endpoints {
            if let Some(index) = self
                .periodic_queue_heads
                .iter()
                .position(|e| core::ptr::addr_eq(e.as_ref(), endpoint))
            {
                self.periodic_queue_heads.remove(index);
                dirty = true;
            } else {
                log::warn!("Queue head at {endpoint:p} was not found",);
            }
        }

        if dirty {
            // Ignore error. There isn't
            self.rebuild_periodic_list().await
        } else {
            Ok(())
        }
    }
}

/// A future to handle waiting on the interrupt on async advance interrupt.
/// Acquiring this sets the doorbell register, `await`ing on this will only return after the
/// interrupt has been received.
struct AsyncDoorbell<'a> {
    controller: &'a Ehci,
    _ticket: async_lock::SemaphoreGuard<'a>,
}

impl<'a> AsyncDoorbell<'a> {
    /// Sets the interrupt on async advance doorbell and returns self.
    fn new(controller: &'a Ehci, sem: async_lock::SemaphoreGuard<'a>) -> Self {
        controller
            .operational_registers
            .usb_command()
            .update(|mut sts| {
                sts.set_int_on_async_doorbell(true);
                sts
            });

        AsyncDoorbell {
            controller,
            _ticket: sem,
        }
    }
}

impl Future for AsyncDoorbell<'_> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self
            .controller
            .operational_registers
            .usb_command()
            .read()
            .get_int_on_async_doorbell()
        {
            self.controller.doorbell_waker.register(cx.waker());
            Poll::Pending
        } else {
            Poll::Ready(())
        }
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

/// The EndpointQueue maintains the state of queued operations for asynchronous jobs.
///
/// The EndpointQueue operates using [TransactionString]'s, which each describes a queued operation.
#[derive(Derivative)]
#[derivative(Ord, PartialEq, PartialOrd, Eq)]
struct EndpointQueueInner {
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
    work: alloc::collections::VecDeque<TransactionString>,
    // Option because this isn't required when transactions are running.
    // This can be used as a cached descriptor
    #[derivative(PartialEq = "ignore")]
    #[derivative(Ord = "ignore")]
    #[derivative(PartialOrd = "ignore")]
    terminator: Option<Box<QueueElementTransferDescriptor, DmaAlloc>>,
}

impl EndpointQueueInner {
    fn new_async(
        target: super::Target,
        pid: super::PidCode,
        packet_size: u32,
        data_toggle_ctl: bool,
    ) -> Self {
        let mut this = Self {
            pid,
            target,
            packet_size,
            head: Box::new_in(
                QueueHead::new(packet_size),
                DmaAlloc::new(hootux::mem::MemRegion::Mem32, 32),
            ),
            work: alloc::collections::VecDeque::new(),
            terminator: Some(Box::new_in(
                QueueElementTransferDescriptor::new(),
                DmaAlloc::new(hootux::mem::MemRegion::Mem32, 32),
            )),
        };
        // This is just to guarantee we have the right type
        let qtd: &mut QueueElementTransferDescriptor = &mut **this.terminator.as_mut().unwrap();
        qtd.set_active(false);
        this.head.data_toggle_ctl(data_toggle_ctl);
        // SAFETY: The address is a valid terminated table.
        unsafe {
            this.head.set_current_transaction(
                hootux::mem::mem_map::translate(qtd as *const _ as usize)
                    .expect("What? Static isn't mapped?") as u32,
            )
        };

        this.head
            .set_target(target.try_into().expect("Invalid target"));
        this
    }

    fn head_of_list() -> Self {
        let mut this = Self::new_async(
            crate::Target {
                dev: crate::DeviceAddress::Default,
                endpoint: crate::Endpoint::new(0).unwrap(),
            },
            crate::PidCode::Control,
            64,
            true,
        );
        this.head.set_head_of_list();
        let qh: &QueueHead = &*this.head;

        this.head.set_next_queue_head(
            hootux::mem::mem_map::translate_ptr(qh)
                .unwrap()
                .try_into()
                .unwrap(),
        );
        this
    }

    fn new_string(
        &mut self,
        payload: DmaBuff,
        int_mode: StringInterruptConfiguration,
    ) -> impl Future<Output = StringCompletion> + use<> {
        let mut st = TransactionString::new(payload, self.packet_size, int_mode);
        let fut = st.get_future();
        if let Some(last) = self.work.get_mut(self.work.len() - 1) {
            // SAFETY: Self ensures that the string is either run to completion or safely removed.
            unsafe { last.append_string(&st) }
            fut
        } else {
            let t = self.terminator.as_mut().unwrap();
            let tgt: &QueueElementTransferDescriptor = &**st.str.last().unwrap();
            let addr = hootux::mem::mem_map::translate_ptr(tgt)
                .unwrap()
                .try_into()
                .unwrap();
            // SAFETY: addr is guaranteed to point to a valid QTD
            unsafe { t.set_next(Some(addr)) };
            fut
        }
    }

    const fn get_target(&self) -> Target {
        self.target
    }

    fn append_cmd_string(
        &mut self,
        mut string: TransactionString,
    ) -> impl Future<Output = StringCompletion> + use<> {
        let fut = string.get_future();

        if let Some(last) = self.work.get_mut(self.work.len() - 1) {
            // SAFETY: `String` is guaranteed to either be completed or safely aborted.
            unsafe { last.append_string(&string) };
        } else {
            assert!(self.is_terminated());
            let head: &QueueElementTransferDescriptor = &**string.str.first().unwrap();
            let head_addr = hootux::mem::mem_map::translate_ptr(head)
                .unwrap()
                .try_into()
                .unwrap();
            // SAFETY: tail_addr is guaranteed to correctly point to a QTD & this only runs when self.work has
            unsafe { self.exit_idle_into(head_addr) };
        }
        self.work.push_back(string);
        fut
    }

    /// Appends a QTD into the work queue when `self` has no work in the work queue.
    ///
    /// # Safety
    ///
    /// The caller must ensure that [Self::is_terminated] returns `true`.
    unsafe fn exit_idle_into(&mut self, qtd_addr: u32) {
        // SAFETY: Guaranteed by caller.
        unsafe { self.head.set_current_transaction(qtd_addr) }
    }

    fn is_terminated(&self) -> bool {
        let initial_addr = self.head.current_qtd();
        // SAFETY: This is used to map an address which we will only read.
        let alloc = unsafe { hootux::alloc_interface::MmioAlloc::new(initial_addr as usize) };

        let addr = alloc
            .allocate(core::alloc::Layout::new::<QueueElementTransferDescriptor>())
            .unwrap()
            .cast::<QueueElementTransferDescriptor>();
        // SAFETY: addr is returned by MmioAlloc with layout of QTD
        let qtd = unsafe { addr.read_volatile() };
        // SAFETY: We can no longer use `addr`
        unsafe {
            alloc.deallocate(
                addr.cast(),
                core::alloc::Layout::new::<QueueElementTransferDescriptor>(),
            )
        };

        let t = &raw const *self.head;
        // SAFETY: Pointer is fetched from reference
        let raw = unsafe { t.read_volatile() };
        if raw.current_qtd() != initial_addr {
            // if the current address changed then clearly we aren't fucking done are we.
            false
        } else {
            !qtd.is_active()
        }
    }

    /// Checks state of all work. Indicates whether we raised an interrupt.
    fn check_state(&mut self) -> bool {
        // SAFETY: Pointer is cast from reference
        let t = unsafe { (&raw const *self.head).read_volatile() };
        let current_addr = t.current_qtd();
        let mut rc = false;

        let mut current = 0u32;
        let mut pull = current;
        for i in self.work.iter_mut() {
            current += 1;
            match i.evaluate_state(current_addr) {
                (TransactionStringState::Completed, brk) => {
                    pull = current;
                    log::trace!("Completion on {:?}", self.target);
                    rc = true;

                    // Replaces `i` with empty transaction string, which is then completed.
                    // Empty TransactionString is will be removed after loop completes.
                    core::mem::replace(i, TransactionString::empty()).complete();
                    if brk {
                        break;
                    }
                }
                (TransactionStringState::Interrupt, brk) => {
                    log::trace!("Interrupt on {:?}", self.target);
                    rc = true;
                    if brk {
                        break;
                    }
                }
                (TransactionStringState::Error, _) => {
                    pull = current;
                    log::error!("USB error on {:?}", self.target);
                    core::mem::replace(i, TransactionString::empty()).complete();
                    rc = true;
                }
                _ => {}
            }
        }
        for _ in 0..pull {
            self.work.pop_front();
        }
        rc
    }
}

pub struct EndpointQueue {
    inner: spin::Mutex<EndpointQueueInner>,
    waiting_for_drop: core::sync::atomic::AtomicBool,
}

impl EndpointQueue {
    fn new_async(
        target: super::Target,
        pid: super::PidCode,
        packet_size: u32,
        data_toggle_ctl: bool,
    ) -> Self {
        Self {
            inner: spin::Mutex::new(EndpointQueueInner::new_async(
                target,
                pid,
                packet_size,
                data_toggle_ctl,
            )),
            waiting_for_drop: core::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Configures a new [PeriodicEndpointQueue]. Functionally this is the same as [Self] but includes extra metadata.
    ///
    /// # Panics
    ///
    /// `target` may not target either [DeviceAddress::Default] or endpoint 0.
    /// `pid` may not be [PidCode::Control].
    fn new_periodic(
        target: Target,
        pid: PidCode,
        packet_size: u32,
        period: u32,
    ) -> PeriodicEndpointQueue {
        assert_ne!(pid, PidCode::Control);
        // SAFETY: Endpoint::new(0) will always return `Some(_)`
        assert_ne!(target.endpoint, unsafe {
            Endpoint::new(0).unwrap_unchecked()
        });
        assert_ne!(target.dev, DeviceAddress::Default);

        let endpoint = Self::new_async(target, pid, packet_size, false);
        PeriodicEndpointQueue {
            endpoint: alloc::sync::Arc::new(endpoint),
            rate: period,
        }
    }

    fn head_of_list() -> Self {
        Self {
            inner: spin::Mutex::new(EndpointQueueInner::head_of_list()),
            waiting_for_drop: core::sync::atomic::AtomicBool::new(false),
        }
    }

    fn update_transaction_len(&self, new_len: u8) -> Result<(), ()> {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut l = self.inner.lock();
            if !l.is_terminated() || l.pid != PidCode::Control {
                return Err(());
            };

            let head = &raw mut *l.head;
            // SAFETY: We guarantee that this QueueHead is currently terminated and that this is
            // indeed an endpoint where we can update this.
            // Pointers are guaranteed to be fine to use, because head is cast from reference.
            unsafe {
                let mut copy = head.read_volatile(); // fixme this can be optimised
                copy.set_max_len(new_len as u32);
                head.write_volatile(copy);
            }
            Ok(())
        })
    }

    pub fn new_string(
        &self,
        payload: DmaBuff,
        int_cfg: StringInterruptConfiguration,
    ) -> impl Future<Output = StringCompletion> + use<> {
        x86_64::instructions::interrupts::without_interrupts(|| {
            self.inner.lock().new_string(payload, int_cfg)
        })
    }

    pub fn get_target(&self) -> Target {
        x86_64::instructions::interrupts::without_interrupts(|| self.inner.lock().get_target())
    }

    fn append_cmd_string(
        &self,
        string: TransactionString,
    ) -> impl Future<Output = StringCompletion> + use<> {
        x86_64::instructions::interrupts::without_interrupts(|| {
            self.inner.lock().append_cmd_string(string)
        })
    }

    fn check_state(&self) -> bool {
        self.inner.lock().check_state()
    }

    fn set_next_endpoint_queue(&self, other: &Self) {
        let addr: &QueueHead = &*other.inner.lock().head;
        let phys_addr = hootux::mem::mem_map::translate_ptr(addr)
            .unwrap()
            .try_into()
            .unwrap();
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut l = self.inner.lock();
            l.head.set_next_queue_head(phys_addr);
        });
    }

    fn terminate_horizontal(&self) {
        let mut l = self.inner.lock();
        l.head.terminate_horizontal(true);
    }

    fn head_addr(&self) -> u32 {
        let addr: *const QueueHead = x86_64::instructions::interrupts::without_interrupts(|| {
            &raw const *self.inner.lock().head
        });
        hootux::mem::mem_map::translate_ptr(addr)
            .unwrap()
            .try_into()
            .unwrap()
    }
}

struct PnpWatchdog {
    controller: alloc::sync::Weak<async_lock::Mutex<Ehci>>,
    ports: Box<[VolatileRef<'static, PortStatusCtl>]>, // ports are owned by self.controller, we must upgrade the controller first.\
    work: alloc::sync::Arc<WorkerWaiter>,
    major_num: MajorNum,
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

            'work_loop: for (i, w) in work_list.iter_mut().enumerate().map(|(i, w)| (i, w.take())) {
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
                        let default_table = ctl_lock.get_default_table();

                        let new_address = ctl_lock.alloc_address();
                        drop(ctl_lock);

                        let command = hootux::mem::dma::DmaBuff::from({
                            Vec::from(usb_cfg::CtlTransfer::set_address(new_address).to_bytes())
                        });

                        let ts = TransactionString::setup_transaction(command, None);
                        if !default_table.append_cmd_string(ts).await.is_ok() {
                            panic!("Failed to set address for device");
                        }

                        let eq = EndpointQueue::new_async(
                            Target {
                                dev: DeviceAddress::Address(new_address.try_into().unwrap()),
                                endpoint: Endpoint::new(0).unwrap(),
                            },
                            PidCode::Control,
                            8,
                            true,
                        );
                        let mut ctl_lock = controller.lock().await;
                        let eq = alloc::sync::Arc::new(eq);
                        ctl_lock.insert_into_async(eq.clone());

                        log::info!("Port {i} initialised as {new_address}");

                        drop(ctl_lock);

                        unsafe {
                            device::UsbDeviceAccessor::insert_into_controller(
                                DeviceAddress::new(new_address).unwrap(),
                                eq,
                                controller.clone(),
                                i as u8,
                            )
                            .await;
                        }
                    }
                    Some(Work::Removed) => {
                        // All we really need to do is drop the handle in the port slot.
                        // Shutdowns will be propagated from there.
                        let mut ctl = controller.lock().await;
                        let file = ctl.port_files.remove(&(i as u8));
                        match file {
                            None => {
                                log::warn!(
                                    "Port {i} was not removed from {:?}, was it not configured?",
                                    hootux::fs::file::DevID::new(self.major_num, 0)
                                );
                                continue 'work_loop;
                            }
                            Some(f) => {
                                let t = f.address;
                                controller.lock().await.free_address(t);
                                // fixme: The address should be freed when the UsbDeviceAccessor is dropped
                                // But that requires AsyncDrop
                                // Could we move the address bitmap outside of the mutex?
                                log::debug!("Device removed: Address freed too early");
                            }
                        }
                        log::info!("usb{}: Port {i} removed", self.major_num.get_raw())
                    }
                }
            }

            drop(controller);

            self.work.wait().await;

            for (i, port) in self.ports.iter_mut().enumerate() {
                let port_sts = port.as_ptr().read();
                if port_sts.connect_status_change() {
                    // Clear other write-one bits and write-back
                    port.as_mut_ptr().write(PortStatusCtl(
                        port_sts.0
                            & !(PortStatusCtl::ACK_OVER_CURRENT | PortStatusCtl::ACK_PORT_ENABLE),
                    ));
                    work_list[i] = if port_sts.connected() {
                        Some(Work::Added)
                    } else {
                        Some(Work::Removed)
                    }
                } else {
                    work_list[i] = None;
                }
            }
        }
    }
}

/// A `TransactionString` describes a series of expected transactions.
/// Due to the limited buffer size of a single [QueueElementTransferDescriptor] a large number
/// of them may be required for a single expected operation.
struct TransactionString {
    meta: TransactionStringMetadata,
    // This can be changed into `Box<[QTD],DmaAlloc>` which may be more optimal
    // current form is more flexible
    // May never have 0 elements
    str: Box<[Box<QueueElementTransferDescriptor, DmaAlloc>]>,
    send: hootux::task::util::MessageFuture<hootux::task::util::Sender, StringCompletion>,
    recv: Option<hootux::task::util::MessageFuture<hootux::task::util::Receiver, StringCompletion>>,
    command_buffer: Option<DmaBuff>,
    data_buffer: DmaBuff,
}

/// Defines how the [TransactionString] should processed on completion.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
enum TransactionStringMetadata {
    Control,
    #[default]
    NormalData,
}

impl TransactionString {
    fn new(
        mut payload: DmaBuff,
        transaction_len: u32,
        interrupt: StringInterruptConfiguration,
    ) -> Self {
        let len = payload.len();
        let offset_into_initial = (&raw const payload[0]) as usize & PAGE_SIZE - 1;

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

        let (tx, rx) = hootux::task::util::new_message::<StringCompletion>();
        Self {
            meta: TransactionStringMetadata::NormalData,
            str: string.into_boxed_slice(),
            send: tx,
            recv: Some(rx),
            command_buffer: None,
            data_buffer: payload,
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

    // Disabled
    /*
    /// Aborts execution of this transaction string by clearing all active bits in the QTDs.
    /// This causes the controller to iterate over them until either the last QTD is reached or
    /// a QTD in another `TransactionString` is reached.
    pub fn abort(&mut self) {
        // Do it in reverse order, to prevent race conditions.
        for i in self.str.iter_mut().rev() {
            i.set_active(false);
        }
    }

     */

    /// Evaluates the state up-to and including the QTD at the address `last_qtd`.
    /// Also emits a bool indicating whether execution has stopped here.
    fn evaluate_state(&self, last_qtd: u32) -> (TransactionStringState, bool) {
        match self.meta {
            TransactionStringMetadata::NormalData => {
                let mut rc = TransactionStringState::None;
                let len = self.str.len();
                for (i, boxed_qtd) in self.str.iter().enumerate() {
                    // SAFETY: Pointer is cast from reference
                    let qtd_ptr = &raw const **boxed_qtd;
                    let qtd = unsafe { qtd_ptr.read_volatile() };
                    let config = qtd.get_config();
                    // if inactive and data remains then we had a short packet, so this string is compplete
                    let qtd_phys_addr =
                        hootux::mem::mem_map::translate_ptr(qtd_ptr).unwrap() as u32;

                    if config.error() {
                        return (TransactionStringState::Error, true); // error always stalls the endpoint
                    } else if !qtd.is_active() && (config.get_expected_size() != 0) {
                        return (TransactionStringState::Completed, false);
                    } else if !qtd.is_active() && config.get_interrupt_on_complete() {
                        log::debug!("Config {config:?}");
                        rc = TransactionStringState::Interrupt
                    } else if config.active() && qtd_phys_addr != last_qtd && i == 0 {
                        // This detects if the last QTD transitioned to the alternate QTD instead of this one.
                        return (TransactionStringState::Completed, false);
                    }
                    // We have checked the last QTD
                    if qtd_phys_addr == last_qtd {
                        return (rc, true);
                    } else if i == len {
                        // this is the last qtd in this string. If we have made it here then this string has been completed.
                        return (TransactionStringState::Completed, false);
                    }
                }
                panic!("Evaluate state managed to escape the loop")
            }
            TransactionStringMetadata::Control => {
                if !self.str.last().unwrap().get_config().active() {
                    return (TransactionStringState::Completed, false);
                } else {
                    for (i, qtd) in self.str.iter().enumerate() {
                        if qtd.is_active() {
                            return (TransactionStringState::None, false);
                        } else if qtd.get_config().error() {
                            log::error!(
                                "Command pipe qTD {i} completed with error - status bits: {:#010b}",
                                qtd.get_config().0 & 0xff
                            );
                            return (TransactionStringState::Error, true);
                        }
                    }
                }
                log::debug!("How did we get here?");
                panic!("How did we get here?")
            }
        }
    }

    fn get_future(&mut self) -> impl Future<Output = StringCompletion> + use<> {
        self.recv.take().expect("Future already taken")
    }

    /// This will set th + use<>e last QTD in the string to raise an interrupt on completion.
    ///
    /// This fn is intended for constructing command strings not normal transaction strings.
    ///
    /// The caller should ensure this is only called when the entire string is assembled,
    /// failure to adhere to this will cause extraneous interrupts.
    fn set_tail_interrupt(&mut self) {
        let Some(last) = self.str.last_mut() else {
            return;
        };
        last.set_int_on_complete()
    }

    fn complete(self) {
        let comp = match self.meta {
            TransactionStringMetadata::Control => {
                // Control always has 3 QTDs only the middle one gets counted.
                let len = self.data_buffer.len()
                    - (self.str[1].get_config().get_expected_size() as usize);
                let mut state = crate::UsbError::Ok;
                for i in self.str {
                    if i.get_config().missed_mocro_frame() {
                        state += crate::UsbError::RecoveredError
                    }
                    if i.get_config().data_buffer_err() {
                        state += crate::UsbError::RecoveredError
                    }
                    if i.get_config().transaction_error() {
                        state += crate::UsbError::RecoveredError
                    }
                    if i.get_config().babbble_detected() {
                        state += crate::UsbError::Halted
                    }
                    if i.get_config().halted() {
                        state += crate::UsbError::Halted
                    }
                }

                StringCompletion {
                    dma_payload: self.data_buffer,
                    command: None,
                    len,
                    state: state.to_result(),
                }
            }
            TransactionStringMetadata::NormalData => {
                let mut len = self.data_buffer.len();
                let mut state = crate::UsbError::Ok;
                for i in self.str {
                    len -= i.get_config().get_expected_size() as usize;
                    if i.get_config().missed_mocro_frame() {
                        state += crate::UsbError::RecoveredError
                    }
                    if i.get_config().data_buffer_err() {
                        state += crate::UsbError::RecoveredError
                    }
                    if i.get_config().transaction_error() {
                        state += crate::UsbError::RecoveredError
                    }
                    if i.get_config().babbble_detected() {
                        state += crate::UsbError::Halted
                    }
                    if i.get_config().halted() {
                        state += crate::UsbError::Halted
                    }
                }
                StringCompletion {
                    dma_payload: self.data_buffer,
                    command: None,
                    len,
                    state: state.to_result(),
                }
            }
        };

        self.send.complete(comp);
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum TransactionStringState {
    /// No state requiring action was found.
    None,
    /// QTD with an interrupt state was found.
    Interrupt,
    /// String has been completed.
    /// This almost definitely indicates an interrupt too
    Completed,
    /// Encountered an error.
    Error,
}

/// Methods for constructing `TransactionString`s for the default command pipe.
impl TransactionString {
    fn empty() -> Self {
        let (tx, rx) = hootux::task::util::new_message();
        Self {
            meta: TransactionStringMetadata::Control,
            str: Box::new([]),
            send: tx,
            recv: Some(rx),
            command_buffer: None,
            data_buffer: DmaBuff::bogus(),
        }
    }

    /// Constructs and appends a single qtd to `self`.
    ///
    /// `data_toggle` indicates the initial state of the data toggle bit.
    /// This setting has no effect when `pid` is [PidCode::Control].
    /// This is required to be set when configuring setup transactions for both the data and status stage.
    ///
    /// # Panics
    ///
    /// `payload.len()` must not exceed the QTD buffer size (20KiB).
    ///
    /// # Safety
    ///
    /// The caller must ensure that `payload` is not dropped before or while the controller
    /// performs DMA to `payload`.
    unsafe fn append_qtd(
        &mut self,
        payload: &mut DmaBuff,
        pid: super::PidCode,
        data_toggle: bool,
    ) -> &mut Self {
        let str = core::mem::take(&mut self.str);
        let mut c = str.into_vec();
        let mut qtd = QueueElementTransferDescriptor::new();
        qtd.data_toggle(data_toggle);
        assert!(payload.len() <= 5 * PAGE_SIZE);

        // needed to extract the offset, we cant get if from the iterator because it gets aligned down into PAGE_SIZE
        let first = payload
            .prd()
            .next()
            .unwrap_or(hootux::mem::dma::PhysicalRegionDescription { addr: 0, size: 0 });
        for (i, region) in payload
            .prd()
            .flat_map(|d| {
                d.adapt_iter(|origin| {
                    // iterates over the aligned down base address
                    let align_down = origin.addr & (PAGE_SIZE - 1) as u64;
                    // This may break down if PAGE_SIZE is not 4096
                    const { assert!(hootux::mem::PAGE_SIZE == PAGE_SIZE) };

                    ((origin.addr - align_down)..(origin.addr + origin.size as u64))
                        .step_by(PAGE_SIZE)
                        .map(|b| hootux::mem::dma::PhysicalRegionDescription {
                            addr: b,
                            size: PAGE_SIZE,
                        })
                })
            })
            .enumerate()
        {
            if i > 4 {
                break;
            } // we cant use buffers larger than 5 pages long

            qtd.set_buffer(i, region.addr)
        }

        qtd.set_offset((first.addr & (PAGE_SIZE as u64) - 1) as u32);
        qtd.set_pid(pid);
        qtd.set_active(true);
        let mut b = Box::<QueueElementTransferDescriptor, _>::new_uninit_in(DmaAlloc::new(
            hootux::mem::MemRegion::Mem32,
            32,
        ));
        qtd.set_data_len(payload.len().try_into().unwrap());
        // SAFETY: MaybeUninit ensures this is aligned.
        // assume_init: Is safe because we initialise `b`
        let b = unsafe {
            b.as_mut_ptr().write_volatile(qtd);
            b.assume_init()
        };

        if let Some(last) = c.last_mut() {
            let qtd: &QueueElementTransferDescriptor = &*b;
            let addr: u32 = hootux::mem::mem_map::translate_ptr(qtd)
                .unwrap()
                .try_into()
                .unwrap();

            // SAFETY: `addr` is guaranteed to point to a valid QTD
            unsafe {
                last.set_next(Some(addr));
            }
        }

        c.push(b);
        self.str = c.into_boxed_slice();
        self
    }

    /// Setup as in the PID not we are setting up a transaction.
    fn setup_transaction(mut setup: DmaBuff, data: Option<(DmaBuff, PidCode)>) -> Self {
        let mut this = Self::empty();
        assert_eq!(
            setup.len(),
            8,
            "Setup transactions must always contain 8 bytes"
        );
        // SAFETY: We guarantee that `setup` owns this address.
        let len = u16::from_le_bytes(setup[6..=7].try_into().unwrap()) as usize;
        // Determines the status PID from `data`
        let status_pid = data
            .as_ref()
            .map(|(_, p)| {
                if *p == PidCode::In {
                    PidCode::Out
                } else {
                    PidCode::In
                }
            })
            .unwrap_or(PidCode::In);

        unsafe {
            this.append_qtd(&mut setup, PidCode::Control, false);
            this.command_buffer = Some(setup);
            if let Some((mut payload, pid)) = data {
                assert_ne!(
                    len, 0,
                    "Setup did not expect a data phase but one was specified anyway"
                );
                assert_ne!(pid, PidCode::Control, "Data PID may not be \"Control\"");
                this.append_qtd(&mut payload, pid, true);
                this.data_buffer = payload;
            }
            // A transaction is used to indicate the setup-command is completed.
            // No data is expected.
            this.append_qtd(&mut DmaBuff::bogus(), status_pid, true);
        };

        this.set_tail_interrupt();
        this
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

// todo: maybe elevate to crate level.
#[must_use]
pub struct StringCompletion {
    dma_payload: DmaBuff,
    command: Option<DmaBuff>,
    len: usize,
    state: Result<crate::UsbError, ()>,
}

impl StringCompletion {
    pub fn complete(self) -> (DmaBuff, usize, Result<crate::UsbError, ()>) {
        (self.dma_payload, self.len, self.state)
    }

    fn is_ok(&self) -> bool {
        self.state.is_ok()
    }

    #[allow(dead_code)]
    fn get_cmd_buffer(&mut self) -> Option<DmaBuff> {
        self.command.take()
    }
}

struct InterruptWorker {
    queue: hootux::task::util::MessageQueue<InterruptMessage, hootux::task::util::Receiver>,
    parent: alloc::sync::Weak<async_lock::Mutex<Ehci>>,
}

impl InterruptWorker {
    async fn run(self: alloc::sync::Arc<Self>) -> hootux::task::TaskResult {
        loop {
            let msg = self.queue.next().await; // unwrap() because this will never close.
            let Some(_ctl) = self.parent.upgrade() else {
                return hootux::task::TaskResult::ExitedNormally;
            };
            match msg {
                InterruptMessage::TransactionStatus => {
                    self.check_usb_work().await;
                }
            }
        }
    }

    async fn check_usb_work(&self) {
        let parent = self.parent.upgrade().unwrap();
        let parent = parent.lock_arc().await;
        let mut found_int = false;
        for i in &parent.async_list {
            if i.check_state() {
                found_int = true
            }
        }

        if !found_int {
            log::trace!("USB spurious interrupt"); // note this is very likely on startup
        }
    }
}

/// This exists to immediately handle interrupts and either perform work immediately or wake a worker.
/// This is required because [Ehci] is managed through a mutex, this contains all required components
/// which can be accessed without requiring a lock to the mutex.
struct IntHandler {
    parent: alloc::sync::Weak<async_lock::Mutex<Ehci>>,
    status_register: VolatilePtr<'static, ehci::operational_regs::UsbStatus>,
    interrupt_worker:
        hootux::task::util::MessageQueue<InterruptMessage, hootux::task::util::Sender>,
    pnp_watchdog: alloc::sync::Weak<WorkerWaiter>,
    poll_period: core::sync::atomic::AtomicU64,
    polling: core::sync::atomic::AtomicBool,
    async_doorbell: alloc::sync::Weak<futures_util::task::AtomicWaker>,
}

impl IntHandler {
    pub fn handle_interrupt(&self) {
        use ehci::operational_regs::UsbStatus;
        let int_status = self.status_register.read();
        // We are using this as a mutex, if we fail to acquire this then we cannot do anything.
        // Also prevents the EHCI from being dropped in operation.
        if let Some(_ehci) = self.parent.upgrade() {
            for i in int_status.int_only().iter() {
                match i {
                    bit @ UsbStatus::USB_INT | bit @ UsbStatus::USB_ERROR_INT => {
                        self.status_register.write(bit);
                        let _ = self
                            .interrupt_worker
                            .send(InterruptMessage::TransactionStatus);
                    }
                    UsbStatus::PORT_CHANGE_DETECT => {
                        self.status_register.write(UsbStatus::PORT_CHANGE_DETECT);
                        let Some(waiter) = self.pnp_watchdog.upgrade() else {
                            continue;
                        }; // What? Are we shutting down? Starting Up?
                        waiter.wake();
                    }
                    UsbStatus::FRAME_LIST_ROLLOVER => {
                        self.status_register.write(UsbStatus::FRAME_LIST_ROLLOVER);
                        // do nothing for now
                    }
                    UsbStatus::HOST_SYSTEM_ERROR => {
                        self.status_register.write(UsbStatus::HOST_SYSTEM_ERROR);
                        panic!("USB error")
                    }
                    UsbStatus::INTERRUPT_ON_ASYNC_ADVANCE => {
                        self.status_register
                            .write(UsbStatus::INTERRUPT_ON_ASYNC_ADVANCE);
                        let Some(w) = self.async_doorbell.upgrade() else {
                            continue;
                        };
                        w.wake();
                    }
                    _ => unreachable!(), // All remaining bits are masked out
                }
            }
        }
    }

    async fn poll(self: alloc::sync::Arc<Self>) -> hootux::task::TaskResult {
        // We use a semaphore to determine whether we need to exit.
        // This is used to indicate if we are currently running, if not a caller can restart this.
        self.polling
            .store(true, core::sync::atomic::Ordering::Relaxed);
        let rc = loop {
            let sleep_period = self.poll_period.load(core::sync::atomic::Ordering::Acquire);
            if sleep_period == 0 {
                break hootux::task::TaskResult::ExitedNormally;
            }
            hootux::task::util::sleep(sleep_period).await;
            let Some(_parent) = self.parent.upgrade() else {
                break hootux::task::TaskResult::ExitedNormally;
            };
            self.handle_interrupt()
        };
        self.polling
            .store(false, core::sync::atomic::Ordering::Release);
        rc
    }
}

enum InterruptMessage {
    TransactionStatus,
}

// SAFETY: auto Send is blocked by VolatilePtr<UsbSts>, but this must be (Send + Sync) to facilitate interrupts.
unsafe impl Send for IntHandler {}
unsafe impl Sync for IntHandler {}

#[derive(Clone)]
pub struct PeriodicEndpointQueue {
    endpoint: alloc::sync::Arc<EndpointQueue>,
    rate: u32,
}

impl PeriodicEndpointQueue {
    fn set_bitmap(&self) {
        let bitmap_sparce = match self.rate {
            0 => unreachable!(),
            n @ 1..8 => n,
            _ => 8,
        };
        let mut bitmap = 0;
        let mut index = 0;
        while index < u8::BITS {
            bitmap |= 1 << index;
            index += bitmap_sparce;
        }

        self.endpoint
            .inner
            .lock()
            .head
            .set_interrupt_schedule_mask(bitmap)
    }
}

impl AsRef<EndpointQueue> for PeriodicEndpointQueue {
    fn as_ref(&self) -> &EndpointQueue {
        &self.endpoint
    }
}
