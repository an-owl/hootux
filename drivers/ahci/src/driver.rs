use crate::PortState;
use ata::command::constructor::{CommandConstructor, MaybeOpaqueCommand, OpaqueCommand};
use core::alloc::Allocator;
use core::fmt::Formatter;
use core::pin::Pin;
use core::task::{Context, Poll};
use hootux::allocator::alloc_interface::MmioAlloc;

mod cmd_ctl;

pub(crate) type HbaInfoRef = alloc::sync::Arc<HbaInfo>;

/// This struct contains HBA specific information that is shared between all components.
/// This allows components to get information that they do not otherwise have direct access to.
pub struct HbaInfo {
    is_64_bit: bool,
    queue_depth: u8,
}

impl HbaInfo {
    pub(crate) fn from_general(ctl: &crate::hba::general_control::GeneralControl) -> Self {
        Self {
            is_64_bit: ctl.get_capabilities().0.supports_qword_addr(),
            queue_depth: ctl.get_capabilities().0.get_command_slots(),
        }
    }

    pub(crate) fn mem_region(&self) -> hootux::mem::buddy_frame_alloc::MemRegion {
        match self.is_64_bit {
            true => hootux::mem::buddy_frame_alloc::MemRegion::Mem64,
            false => hootux::mem::buddy_frame_alloc::MemRegion::Mem32,
        }
    }

    /// Returns the number of command slots allowed per port. Command slots `0..Self.queue_depth`.
    /// This will never return 0 or a value above 32, this means regardless of the returned value
    /// slot 0 will always be usable
    fn queue_depth(&self) -> u8 {
        self.queue_depth
    }
}

pub struct AbstractHba {
    info: HbaInfoRef,
    general: &'static mut crate::hba::general_control::GeneralControl,
    ports: [Option<Port>; 32],
    state: [atomic::Atomic<PortState>; 32],
}

impl AbstractHba {
    /// Initializes the HBA from the given memory region.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the given location points to a AHCI HBA configuration region,
    /// and uses an appropriate caching mode.
    pub unsafe fn new(region: alloc::boxed::Box<[u8; 2048], MmioAlloc>) -> Self {
        let r = alloc::boxed::Box::leak(region);
        let hba = crate::hba::HostBusAdapter::from_raw(r as *mut _ as *mut u8);
        let crate::hba::HostBusAdapter {
            general,
            vendor: _vendor,
            ports,
        } = hba;

        general.claim_from_firmware();

        let info = alloc::sync::Arc::new(HbaInfo::from_general(general));
        let mut raw_ports = ports.each_mut().map(|p| Some(p));

        let mut ports = [const { None }; 32];

        for (i, p) in raw_ports.iter_mut().enumerate() {
            if general.check_port(i as u8) {
                ports[i] = Some(Port::new(p.take().unwrap(), info.clone()))
            }
        }

        for i in &ports {
            if let Some(p) = i {
                p.port
                    .lock()
                    .interrupt_status
                    .clear(crate::hba::port_control::InterruptStatus::all());

                p.enable(true);
            }
        }

        let ret = Self {
            info,
            general,
            ports,
            state: core::array::from_fn(|_| atomic::Atomic::new(PortState::NotImplemented)),
        };
        ret.update_state();

        ret
    }

    fn update_state(&self) {
        for (p, s) in core::iter::zip(&self.ports, &self.state) {
            // this way reduces the number of atomic ops used
            if let Some(n) = p.as_ref().map(|p| p.get_state()) {
                s.store(n, atomic::Ordering::Relaxed)
            }
        }
    }

    /// Returns a reference to the given port.
    ///
    /// # Panics
    ///
    /// This fn will panic if `port > 31` or if the port is not implemented.
    pub(crate) fn get_port(&self, port: u8) -> &Port {
        &self.ports[port as usize].as_ref().unwrap()
    }

    pub(crate) async fn chk_ports(&self) {
        for i in &self.ports {
            if let Some(p) = i {
                p.refresh_exec().await;
            }
        }
    }
}

impl Drop for AbstractHba {
    fn drop(&mut self) {
        // todo: ensure that other CPUs haven't locked self.
        // do this by taking self into its own task, and checking all mutexes and commands
        // until they are free. Poll until true.
        let t = { self.general as *mut _ as *mut [u8; 2048] };
        let a = unsafe { MmioAlloc::new(0) };

        // ports must be dropped because they reference the HBA struct.
        for i in &mut self.ports {
            drop(i.take());
        }

        // SAFETY: References (apart from general) are dropped.
        unsafe {
            a.deallocate(
                core::ptr::NonNull::new(t).unwrap().cast(),
                core::alloc::Layout::from_size_align(2048, 2048).unwrap(),
            );
        }
    }
}

pub struct Port {
    info: HbaInfoRef,
    identity: spin::Mutex<Option<DevIdentity>>,
    // I dont think this actually needs to be a mutex.
    // The vast majority of accesses to this are reads.
    // As long as the driver is working correctly I dont think multiple threads can actually access
    // this simultaneously.
    // use mutex for now, maybe change to cell later and impl Sync !Send for Self
    pub(crate) port: spin::Mutex<&'static mut crate::hba::port_control::PortControl>,
    // memory is allocated by self but is not owned.
    cmd_tables: cmd_ctl::CmdList,
    active_cmd_fut: [spin::Mutex<Option<CmdFuture>>; 32],
    cmd_lock: CmdLock,
    cmd_queue: spin::Mutex<alloc::collections::VecDeque<CmdFuture>>,
}

impl Port {
    fn new(port: &'static mut crate::hba::port_control::PortControl, info: HbaInfoRef) -> Self {
        let tables = cmd_ctl::CmdList::new(info.clone());
        port.set_cmd_table(tables.table_addr());

        Self {
            info,
            identity: spin::Mutex::new(None),
            port: spin::Mutex::new(port),
            cmd_tables: tables,
            active_cmd_fut: core::array::from_fn(|_| spin::Mutex::new(None)),
            cmd_lock: CmdLock::new(),
            cmd_queue: spin::Mutex::new(alloc::collections::VecDeque::new()),
        }
    }

    /// Returns the device identity information. This may require fetching it from the device if it
    /// is not currently present.
    pub fn get_identity(&self) -> futures::future::BoxFuture<DevIdentity> {
        use futures::FutureExt;
        async {
            // using if let here does not free the mutex until after the if/else returns
            // this prevents deadlocks
            let oi = self.identity.lock().clone();

            if let Some(i) = oi {
                log::info!("{:#x?}", i);
                i
            } else {
                // min alignment for alloc is 8. This should be u16
                let mut buffer = alloc::boxed::Box::new([0u8; 512]);

                // SAFETY: IdentifyDevice returns a 512 byte buff
                unsafe {
                    self.issue_cmd(
                        ata::command::constructor::NoArgCmd::IdentifyDevice.compose(),
                        Some(&mut buffer[..]),
                    )
                    .await
                    .unwrap(); // todo handle this
                }

                let id_raw: alloc::boxed::Box<ata::structures::identification::DeviceIdentity> =
                    unsafe { core::mem::transmute(buffer) };

                let id = DevIdentity::from(*id_raw);

                *self.identity.lock() = Some(id);
                id
            }
        }
        .boxed()
    }

    fn enable(&self, state: bool) {
        self.port.lock().cmd_status.update(|t| {
            t.set(crate::hba::port_control::CommStatus::START, state);
        })
    }

    /// Attempts to issue the command to the device. If the command list is full the command will be queued.
    pub async unsafe fn issue_cmd(
        &self,
        cmd: ata::command::constructor::ComposedCommand,
        buff: Option<&mut [u8]>,
    ) -> Result<Option<&mut [u8]>, CmdErr> {
        let fut = self.construct_future(cmd, buff);

        // this probably won't end up waiting
        if let None = self.exec_cmd(fut.clone()).await {
            self.queue_command(fut.clone()).await;
        }

        // SAFETY: This is safe because the buffer is originally given as a ref.
        // The deref must occur because the buffer may not have an associated lifetime.
        fut.await.map(|k| k.map(|b| unsafe { &mut *b }))
    }

    /// Attempts to send the cmd to the device Returns the command slot used.
    /// If all command slots are used returns None
    ///
    /// # Safety
    ///
    /// The caller must ensure that the buffer is correctly sized for the given command.
    async unsafe fn exec_cmd(&self, cmd: CmdFuture) -> Option<u8> {
        let slot = self.cmd_lock.get_cmd()?;
        let (fis, nqc) = if let Some(ret) = self.compile_fis(&cmd.data.cmd) {
            ret
        } else {
            self.get_identity().await;
            // Should never panic None can only be returned if the device identity is not present
            self.compile_fis(&cmd.data.cmd).unwrap()
        };

        let mut table = self.cmd_tables.table(slot).unwrap(); // panics here are a bug. Did self.info change?
        let b = cmd.data.get_buff();

        // actually sends the command to the device.
        // fixme errors here should be handled
        table.send_fis(fis, b).expect("fixme");
        *self.active_cmd_fut[slot as usize].lock() = Some(cmd.clone());

        // disables interrupts to minimize time between command start and updating the lock state.
        // could use x86_64 crate but i dont really want that for this.
        // todo make these arch generic
        core::arch::asm!("cli", options(nomem, nostack));

        // SAFETY: The RPDT is set up and the FIS is valid.
        if nqc {
            self.port.lock().exec_nqc(slot)
        } else {
            self.port.lock().exec_cmd(slot)
        }
        self.cmd_lock.full_lock(slot);

        core::arch::asm!("sti", options(nomem, nostack));

        Some(slot)
    }

    /// Pushes the command onto the waiting queue. This will immediately attempt to execute the
    /// command if any free command slots are available.
    async unsafe fn queue_command(&self, cmd: CmdFuture) {
        self.cmd_queue.lock().push_back(cmd);
        // probably wont wait
        self.refresh_exec().await
    }

    /// Checks for completed commands, wakes them and attempts to issue new commands from the queue.
    /// This may require waiting for [Self::get_identity]. This fn does not wait for command completion.
    async fn refresh_exec(&self) {
        // only use implemented command slots
        for i in 0..self.info.queue_depth() {
            if self.chk_complete(i) {
                // wake future and clear lock.
                let c = self.active_cmd_fut[i as usize].lock().take().expect("Race");
                c.ready();
                self.cmd_lock.free(i);

                if let Some(c) = self.cmd_queue.lock().pop_front() {
                    // SAFETY: In theory everything has been checked before it gets here
                    unsafe { self.exec_cmd(c).await };
                }
            }
        }
    }

    /// Checks if the given command has been completed.
    fn chk_complete(&self, cmd: u8) -> bool {
        let t = self.cmd_lock.cmd[cmd as usize].load(atomic::Ordering::Relaxed);
        if t != CmdLockState::Running {
            return false;
        }
        let s = self.port.lock().cmd_state();
        s & (1 << cmd) == 0
    }

    fn construct_future(
        &self,
        cmd: ata::command::constructor::ComposedCommand,
        buff: Option<&mut [u8]>,
    ) -> CmdFuture {
        CmdFuture {
            data: alloc::sync::Arc::new(CmdDataInner {
                cmd,
                state: atomic::Atomic::new(CmdState::Queued),
                buff: atomic::Atomic::new(buff.map(|b| b as *mut [u8])),
                waker: Default::default(),
            }),
        }
    }

    /// Converts a composed command into a Frame Information Structure which can be sent to the device.
    /// This fn will return `None` when the device identity is required but not present.
    /// When this occurs [Self::get_identity] must be called.
    /// This fn will also return whether or not the FIS uses a NQC command.
    ///
    /// None may be returned under various circumstances including when the command isn't supported.
    /// If the caller fails to handle this it should signal `AtaErr`  
    // this is needed because the required command may change e.g if a non nqc command is requested
    // while NQC is active, all ops must change to non NQC.
    fn compile_fis(
        &self,
        cmd: &ata::command::constructor::ComposedCommand,
    ) -> Option<(
        crate::hba::command::frame_information_structure::RegisterHostToDevFis,
        bool,
    )> {
        match cmd.command {
            MaybeOpaqueCommand::Concrete(c) => Some((cmd.try_into().unwrap(), c.is_nqc())), // this never panics on Concrete(_)
            MaybeOpaqueCommand::Opaque(c) => {
                let mut command = cmd.clone();
                // todo this isn't permanent
                command.command = MaybeOpaqueCommand::Concrete(match c {
                    OpaqueCommand::Read => ata::command::AtaCommand::READ_DMA_EXT,
                    OpaqueCommand::Write => ata::command::AtaCommand::WRITE_DMA_EXT,
                });

                let ncq = if let MaybeOpaqueCommand::Concrete(c) = &command.command {
                    c.is_nqc()
                } else {
                    unreachable!()
                };

                Some(((&command).try_into().unwrap(), ncq))
            }
        }
    }

    fn get_state(&self) -> PortState {
        self.port.lock().get_port_state()
    }
}

impl core::fmt::Debug for Port {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.port, f)
    }
}

#[derive(Copy, Clone, Debug)]
/// This struct contains device identification information
pub struct DevIdentity {
    /// Number of bytes per logical sector. A logical sector is the smallest value that the device
    /// can access. Commands that access the physical medium must be aligned to this value.
    /// This should never exceed `0x20000`  bytes
    lba_size: u64,
    /// Number of bytes in the physical sector. Operations prefer to be aligned to this over `lba_size` for optimal performance.
    /// This number is always a multiple of `lba_size`
    phys_sec_size: u64,
    /// LBA offset. This value is the offset of hte first LBA into the first physical sector.
    /// For an LBA size of 512 and a physical sector size of 4096 if this value is 1 the first lba
    /// is byte 512 of the first physical sector.
    ///
    /// For optimal performance commands should start and end at physical sector boundaries.
    offset: u16,
    /// Total number of LBAs on the device. Commands may never exceed this LBA. This has a maximum value of `0xFFFF_FFFF_FFFF`
    lba_count: u64,
}

impl From<ata::structures::identification::DeviceIdentity> for DevIdentity {
    fn from(value: ata::structures::identification::DeviceIdentity) -> Self {
        let g = value.get_device_geometry();
        Self {
            lba_size: g.logical_sec_size() as u64,
            phys_sec_size: g.phys_sec_size(),
            offset: g.get_alignment(),
            lba_count: g.lba_count(),
        }
    }
}

/// This struct is to allow for the asynchronous locking of command slots by the driver.
///
/// When a thread locates a free command it must set up a FIS and send the command before the CI bit
/// is set, during this time another thread may attempt to use this command slot. The entire port
/// can be placed within a mutex but this may have a high performance impact.
/// This struct minimizes this performance impact by only spinning while a free command is located.
/// This struct also allows command slots to be freed regardless of the lock.
struct CmdLock {
    lock: core::sync::atomic::AtomicBool,
    cmd: [atomic::Atomic<CmdLockState>; 32],
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
enum CmdLockState {
    /// Command is not locked and may not be changed without acquiring the outer lock.
    Unlocked,
    /// Command is locked but not issued. Command completion checks should ignore any command slots
    /// with this state. THe outer mutex is not required to change this state.
    Setup,
    /// Command is locked and issued to the device. Not active commands from the PxCI register where
    /// this variant is the current state are completed. The outer lock is not required to change this state.   
    Running,
}

impl CmdLock {
    fn new() -> Self {
        Self {
            lock: core::sync::atomic::AtomicBool::new(false),
            cmd: core::array::from_fn(|_| atomic::Atomic::new(CmdLockState::Unlocked)),
        }
    }
    /// Locks the first free command slot and returns it's index.
    fn get_cmd(&self) -> Option<u8> {
        while let Err(_) = self.lock.compare_exchange_weak(
            false,
            true,
            atomic::Ordering::Acquire,
            atomic::Ordering::Relaxed,
        ) {
            core::hint::spin_loop();
        }

        let mut c = None;
        for (i, p) in self.cmd.iter().enumerate() {
            if CmdLockState::Unlocked == p.load(atomic::Ordering::Relaxed) {
                p.store(CmdLockState::Setup, atomic::Ordering::Relaxed);
                c = Some(i as u8);
                break;
            }
        }
        self.lock.store(false, atomic::Ordering::Release);
        c
    }

    /// Changes the lock state from [CmdLockState::Setup] to [CmdLockState::Running].
    ///
    /// # Panics
    ///
    /// This fn will panics if the given `cmd` is not [CmdLockState::Setup]
    fn full_lock(&self, cmd: u8) {
        self.cmd[cmd as usize]
            .compare_exchange_weak(
                CmdLockState::Setup,
                CmdLockState::Running,
                atomic::Ordering::Release,
                atomic::Ordering::Relaxed,
            )
            .expect("ACHI: Command not locked");
    }

    /// Frees the given cmd without acquiring does not acquire the lock.
    fn free(&self, cmd: u8) {
        self.cmd[cmd as usize].store(CmdLockState::Unlocked, atomic::Ordering::Release);
    }
}

/// Contains the inner data for a future used to issue and receive commands.
///
/// # Safety
///
/// Buff must never be dereferenced. It is stored exclusively to access the pointer/len not the
/// inner data. Deferencing the data would break the `Sync` and `Send`.
struct CmdDataInner {
    cmd: ata::command::constructor::ComposedCommand,
    state: atomic::Atomic<CmdState>,
    buff: atomic::Atomic<Option<*mut [u8]>>,
    waker: futures::task::AtomicWaker,
}

impl CmdDataInner {
    fn take_buff(&self) -> Option<*mut [u8]> {
        // should be non locking due to non-zero optimization
        self.buff.swap(None, atomic::Ordering::Relaxed)
    }

    fn get_buff(&self) -> Option<*mut [u8]> {
        self.buff.load(atomic::Ordering::Relaxed)
    }
}

unsafe impl Send for CmdDataInner {}
unsafe impl Sync for CmdDataInner {}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum CmdState {
    /// The command is queued and waiting to be issued to the device
    Queued,
    /// The Command is currently being processed by the device.
    InProgress,
    /// The command has completed is awaiting finalization.
    Completed,
    /// An error was detected. The data within the buffer may or may not be ready.
    Err(CmdErr),
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum CmdErr {
    /// An error with the command was detected. This this variant means the attempted command was
    /// declared erroneous by the driver.
    AtaErr,
    /// The device signaled completion with an error.
    DevErr,
    /// The system is no longer in communication with the device.
    Disowned,
    /// An error was encountered while building the command. This differs from [Self::AtaErr] because
    /// the command itself was logically correct but an issue was encountered while building the command.
    BuildErr(cmd_ctl::CommandError),
}

#[derive(Clone)]
struct CmdFuture {
    data: alloc::sync::Arc<CmdDataInner>,
}

impl CmdFuture {
    fn ready(&self) {
        self.data
            .state
            .store(CmdState::Completed, atomic::Ordering::Relaxed);
        self.data.waker.wake();
    }

    fn err(&self, err: CmdErr) {
        self.data
            .state
            .store(CmdState::Err(err), atomic::Ordering::Relaxed);
        self.data.waker.wake();
    }
}

impl core::future::Future for CmdFuture {
    type Output = Result<Option<*mut [u8]>, CmdErr>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.data.state.load(atomic::Ordering::Relaxed) {
            CmdState::Completed => Poll::Ready(Ok(self.data.take_buff())),

            CmdState::Err(e) => Poll::Ready(Err(e)),

            _ => {
                self.data.waker.register(cx.waker());
                Poll::Pending
            }
        }
    }
}
