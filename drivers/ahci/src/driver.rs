use crate::hba::port_control::InterruptStatus;
use crate::PortState;
use ata::command::constructor::{CommandConstructor, MaybeOpaqueCommand, OpaqueCommand};
use core::alloc::Allocator;
use core::fmt::Formatter;
use core::pin::Pin;
use core::task::{Context, Poll};
use hootux::alloc_interface::MmioAlloc;

mod cmd_ctl;
pub(crate) mod kernel_if;

pub(crate) type HbaInfoRef = alloc::sync::Arc<HbaInfo>;

/// This struct contains HBA specific information that is shared between all components.
/// This allows components to get information that they do not otherwise have direct access to.
#[derive(Debug, Copy, Clone)]
pub struct HbaInfo {
    is_64_bit: bool,
    queue_depth: u8,
    mech_presence_switch: bool,
    pci_addr: hootux::system::pci::DeviceAddress,
}

impl HbaInfo {
    pub(crate) fn from_general(
        ctl: &crate::hba::general_control::GeneralControl,
        pci_addr: hootux::system::pci::DeviceAddress,
    ) -> Self {
        Self {
            is_64_bit: ctl.get_capabilities().0.supports_qword_addr(),
            queue_depth: ctl.get_capabilities().0.get_command_slots(),
            mech_presence_switch: ctl
                .get_capabilities()
                .0
                .contains(crate::hba::general_control::HbaCapabilities::PRESENCE_SWITCH),
            pci_addr,
        }
    }

    pub(crate) fn mem_region(&self) -> hootux::mem::MemRegion {
        match self.is_64_bit {
            true => hootux::mem::MemRegion::Mem64,
            false => hootux::mem::MemRegion::Mem32,
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
    pub unsafe fn new(region: &mut [u8], bus_addr: hootux::system::pci::DeviceAddress) -> Self {
        let r = region;
        let hba = crate::hba::HostBusAdapter::from_raw(r);
        let crate::hba::HostBusAdapter {
            general,
            vendor: _vendor,
            ports,
        } = hba;

        general.claim_from_firmware();

        let info = alloc::sync::Arc::new(HbaInfo::from_general(general, bus_addr));
        let mut raw_ports: alloc::vec::Vec<Option<&mut crate::hba::port_control::PortControl>> =
            ports.into_iter().map(|p| Some(p)).collect();

        let mut ports = [const { None }; 32];

        // unimplemented ports stay None
        for (i, p) in raw_ports.iter_mut().enumerate() {
            if general.check_port(i as u8) {
                ports[i] = Some(Port::new(p.take().unwrap(), info.clone(), i as u8))
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

    /// Refreshes execution on all ports that raised an interrupt.
    pub(crate) async fn chk_ports(&self) {
        let t = self.general.int_status();
        self.general.clear_int(t);
        for i in 0..32 {
            if t & 1 << i != 0 {
                self.ports[i].as_ref().expect("CCC Bug?").update().await;
            }
        }
    }

    pub fn info(&self) -> HbaInfo {
        *self.info
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
    index: u8,
    info: HbaInfoRef,
    // see port comment
    known_state: atomic::Atomic<PortState>,
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
    err_chk: PortErrChk,
}

impl Port {
    fn new(
        port: &'static mut crate::hba::port_control::PortControl,
        info: HbaInfoRef,
        index: u8,
    ) -> Self {
        let tables = cmd_ctl::CmdList::new(info.clone());
        port.set_cmd_table(tables.table_addr());

        Self {
            index,
            info,
            known_state: atomic::Atomic::new(port.get_port_state()),
            identity: spin::Mutex::new(None),
            port: spin::Mutex::new(port),
            cmd_tables: tables,
            active_cmd_fut: core::array::from_fn(|_| spin::Mutex::new(None)),
            cmd_lock: CmdLock::new(),
            cmd_queue: spin::Mutex::new(alloc::collections::VecDeque::new()),
            err_chk: PortErrChk::new(),
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

                if id.lba_count == 0 {
                    debug_assert!(id.lba_count == 0, "id bad");
                    log::error!("id bad");
                }
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
    // todo: This fn should construct the PRDT. ATM it is only constructed when a command slot is free, doing it here will minimize command downtime.
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
        // not allowed to run commands while in error state
        if self.err_chk.is_err() {
            return None;
        }

        let slot = self.cmd_lock.get_cmd()?;
        let (fis, nqc) = if let Some(ret) = self.compile_fis(&cmd.data.cmd) {
            cmd.data.nqc.store(true, atomic::Ordering::Relaxed);
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

        self.port.lock().tfd_wait();

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
        if !self.err_chk.is_err() {
            let tfd;
            let ci;
            {
                let l = self.port.lock();
                tfd = l.task_file_data.read();
                ci = l.get_ci();
            }

            if let Some(c) = self.err_chk.chk(tfd, &self.cmd_lock, ci) {
                // should never panic. If it does then exec_cmd() probably isn't working properly
                let c = self.active_cmd_fut[c as usize].lock().take().unwrap();
                c.err(CmdErr::DevErr(tfd.get_err()));
            }

            if !self.err_chk.is_err() {
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
            } else {
                self.err_refresh();
            }
        } else {
            self.err_refresh();
        }
    }

    /// Enter error handling state. Each command will be retried one by one, if the command returns
    /// another error it will be aborted.
    /* todo currently transmission errors will abort a single command.
      atm im not too sure how to deal with this
      data errors should have a certain threshold before declaring a device as failed
      but I'm not too sure how to handle this.
      should it be fails:time ratio or success:fail ratio
      if a single command fails 10 times but others succeed should just that command be aborted?
    */
    fn err_refresh(&self) {
        // cannot run error check while commands are active.
        if self.port.lock().get_ci() != 0 {
            self.err_chk.inner.lock().waiting = true;
            return;
        }
        self.err_chk.inner.lock().waiting = false;

        let done = self.complete().trailing_zeros();
        let err = self.port.lock().task_file_data.read().get_status();

        // take because the future is now concluded
        let fut = self.active_cmd_fut[done as usize].lock().take().unwrap();
        if err == 0 {
            fut.ready()
        } else {
            fut.err(CmdErr::DevErr(err));
        }

        let n = self.err_chk.next();

        // SAFETY: The safety guarantees must have been checked to reach this point.
        if self.active_cmd_fut[n as usize]
            .lock()
            .as_ref()
            .unwrap()
            .data
            .nqc
            .load(atomic::Ordering::Relaxed)
        {
            unsafe { self.port.lock().exec_nqc(n) }
        } else {
            unsafe { self.port.lock().exec_cmd(n) }
        }
    }

    /// Returns which commands are completed
    fn complete(&self) -> u32 {
        let mut ret = 0;
        let ci = self.port.lock().get_ci();
        for (i, s) in self.cmd_lock.cmd.iter().enumerate() {
            let s = s.load(atomic::Ordering::Relaxed);
            let mask = 1 << i;
            if s == CmdLockState::Running && ci & mask == 0 {
                ret |= mask;
            }
        }
        ret
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
                state: atomic::Atomic::new(CmdState::Waiting),
                buff: atomic::Atomic::new(buff.map(|b| b as *mut [u8])),
                waker: Default::default(),
                nqc: atomic::Atomic::new(false),
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
        let mut cmd = cmd.clone();
        match cmd.command {
            MaybeOpaqueCommand::Concrete(c) => Some(((&cmd).try_into().unwrap(), c.is_nqc())), // this never panics on Concrete(_)
            MaybeOpaqueCommand::Opaque(c) => {
                let mut command = cmd.clone();
                // todo this isn't permanent
                command.command = MaybeOpaqueCommand::Concrete(match c {
                    OpaqueCommand::Read => {
                        let dev = cmd.device.unwrap_or(0) | (1 << 6);
                        // required for READ_DMA_EXT
                        command.device = Some(dev);
                        ata::command::AtaCommand::READ_DMA_EXT
                    }
                    OpaqueCommand::Write => {
                        let dev = cmd.device.unwrap_or(0) | (1 << 6);
                        // required for WRITE_DMA_EXT
                        command.device = Some(dev);
                        ata::command::AtaCommand::WRITE_DMA_EXT
                    }
                });

                // ensures that fields are compatible with with their commands
                // this will require more building out
                if c == OpaqueCommand::Read || c == OpaqueCommand::Write && !command.is_48_bit() {
                    match cmd.count.expect("Caught disk IO command without count") {
                        256 => cmd.count = Some(0),
                        n if n > 256 => return None,
                        _ => {}
                    }
                    if cmd.lba.expect("Caught disk IO without LBA") > 0xFFFFFFF {
                        return None;
                    }
                }

                let ncq = if let MaybeOpaqueCommand::Concrete(c) = &command.command {
                    c.is_nqc()
                } else {
                    unreachable!()
                };

                Some(((&command).try_into().unwrap(), ncq))
            }
        }
    }

    /// Reads from the device at `lba` for `len` sectors. The buffer is automatically sized to contain
    /// the read data. The size of the buffer can be calculated using the data returned by [Self::get_identity]
    ///
    /// This fn will return Err(BadArgs) if `lba >= 0x1000000000000`
    /// or if the given lba + count exceeds the last sector on the device.
    pub async fn read(
        &self,
        lba: SectorAddress,
        count: SectorCount,
    ) -> Result<alloc::boxed::Box<[u8]>, CmdErr> {
        use ata::command::constructor;

        let id = self.get_identity().await;
        if id.exceeds_dev(lba, count) {
            return Err(CmdErr::BadArgs);
        }

        let mut b = alloc::vec::Vec::new();
        b.resize(id.lba_size as usize * count.count_ext() as usize, 0);
        let mut b = b.into_boxed_slice();

        let c = constructor::SpanningCmd::new(
            constructor::SpanningCmdType::Read,
            lba.raw(),
            count.count_ext(),
        )
        .ok_or(CmdErr::BadArgs)?;

        // SAFETY: This is safe because the command take a buffer and the buffer size is equal to
        // the size of the expected data.
        unsafe { self.issue_cmd(c.compose(), Some(&mut *b)) }.await?;
        Ok(b)
    }

    /// This fn writes the given buffer to the device at starting at `lba`.
    ///
    /// This fn will return Err(BadArgs) if the size of given buffer is not aligned to the logical
    /// sector size of the device or the buffer + `lba` exceeds the size of the device.
    pub async fn write(&self, lba: SectorAddress, buff: &mut [u8]) -> Result<(), CmdErr> {
        use ata::command::constructor;

        let id = self.get_identity().await;
        if buff.len() as u64 % id.lba_size != 0 {
            return Err(CmdErr::BadArgs);
        }

        let count = SectorCount::new(
            (buff.len() as u64 / id.lba_size)
                .try_into()
                .ok()
                .ok_or(CmdErr::BadArgs)?,
        )
        .ok_or(CmdErr::BadArgs)?;

        if id.exceeds_dev(lba, count) || buff.len() == 0 {
            return Err(CmdErr::BadArgs);
        }

        let c = constructor::SpanningCmd::new(
            constructor::SpanningCmdType::Write,
            lba.raw(),
            count.count_ext(),
        )
        .ok_or(CmdErr::BadArgs)?;
        // SAFETY: This is safe because the the count has been calculated from the size of the buffer.
        unsafe { self.issue_cmd(c.compose(), Some(buff)) }.await?;
        Ok(())
    }

    fn get_state(&self) -> PortState {
        self.port.lock().get_port_state()
    }

    /// Sets the enabled interrupts for the port.
    ///
    /// If any bits are not allowed they will be returned and the interrupt status will not be updated.
    unsafe fn set_int_enable(
        &self,
        ie: crate::hba::port_control::InterruptEnable,
    ) -> Result<(), crate::hba::port_control::InterruptEnable> {
        // check cold presence(cmd.cpd), mechanical presence (cap1)
        use crate::hba::port_control::InterruptEnable;
        let mut err = Ok(());
        if ie.contains(InterruptEnable::COLD_PORT_DETECT) {
            if !self
                .port
                .lock()
                .cmd_status
                .read()
                .contains(crate::hba::port_control::CommStatus::COLD_PRESENCE_DETECTION)
            {
                err = Err(InterruptEnable::COLD_PORT_DETECT);
            }
        }
        if ie.contains(InterruptEnable::DEVICE_MECHANICAL_PRESENCE) {
            if !self.info.mech_presence_switch {
                if let Err(i) = &mut err {
                    *i |= InterruptEnable::DEVICE_MECHANICAL_PRESENCE;
                } else {
                    err = Err(InterruptEnable::DEVICE_MECHANICAL_PRESENCE);
                }
            }
        }

        err?; //returns on err
        self.port.lock().interrupt_enable.write(ie);
        Ok(())
    }

    /// Attempts to handle the ports current interrupt status.
    /// An interrupt does **not** need to be raised to call this fn.
    ///
    /// - If the port status is changed it will be updated and [Self::state_update] will be called.
    /// - If a fatal data error is detected all commands will be restarted
    /// - If a task file error was detected [Self::err_refresh] will be called. Determining which command caused the error and completing all others.
    /// - If a command completion was detected this fn will return true and [Self::refresh_exec] must be called.
    fn handle_int(&self) -> bool {
        let is = {
            let l = self.port.lock();
            let is = l.interrupt_status.read();
            is
        };

        if is.intersects(
            InterruptStatus::COLD_PORT_DETECT
                | InterruptStatus::DEVICE_MECHANICAL_PRESENCE
                | InterruptStatus::PORT_CONNECT_CHANGE,
        ) {
            self.int_clear(InterruptStatus::all());
            self.state_update();
            return false;
        }

        let chk = InterruptStatus::INTERFACE_FATAL;
        if is.contains(chk.clone()) {
            // clear should be called first. In case more occur.
            self.int_clear(chk);

            // retry all commands
            for (i, s) in self.cmd_lock.cmd.iter().enumerate() {
                let mut l = 0;
                if s.load(atomic::Ordering::Relaxed) == CmdLockState::Running {
                    l |= 1 << i;
                }
                self.port.lock().set_ci(l);
            }
            log::warn!("{self}: Interface fatal data error detected, Retrying");

            // basically clear anything that will be set by a command
            self.int_clear(
                InterruptStatus::INTERFACE_FATAL
                    | InterruptStatus::DESCRIPTOR_PROCESSED
                    | InterruptStatus::TASK_FILE_ERROR
                    | InterruptStatus::INTERFACE_NON_FATAL
                    | InterruptStatus::DMA_SETUP_FIS
                    | InterruptStatus::OVERFLOW
                    | InterruptStatus::PIO_SETUP_FIS,
            );
        }

        let chk = InterruptStatus::TASK_FILE_ERROR;
        if is.contains(chk.clone()) {
            self.err_refresh();
            self.int_clear(chk.clone() | InterruptStatus::DEV_TO_HOST_FIS); // its unknown which command returned err so they must all be retried
            return false;
        }
        let chk = InterruptStatus::DEV_TO_HOST_FIS;
        if is.contains(chk.clone()) {
            return true;
        }
        return false;
    }

    /// Handles unexpected device state updates.
    fn state_update(&self) {
        let ks = self.known_state.load(atomic::Ordering::Relaxed);
        match self.get_state() {
            PortState::NotImplemented => unreachable!(), // get_state() cannot return this
            PortState::None => {
                self.abandon_cmd();
                self.known_state
                    .store(PortState::None, atomic::Ordering::Relaxed);
            }
            PortState::Cold => {
                if ks == PortState::Hot {
                    // port has become unavailable
                    log::warn!("AHCI: {} has been detected via Cold Presence Detection without being removed", self);
                    self.abandon_cmd();
                } else if ks == PortState::Warm {
                    log::warn!("AHCI: {} has been detected via Cold Presence Detection without being removed", self);
                }

                todo!()
                // notify system & wait for input?
                // identify the device and leave it?
            }
            PortState::Warm => todo!(), // not sure what to do here because im pretty sure this would be software controlled
            PortState::Hot => todo!(),  // add new block device
        }
    }

    /// Abandons all commands.
    ///
    /// This should be called in the event that the device becomes unavailable.
    fn abandon_cmd(&self) {
        for i in &self.active_cmd_fut {
            let t = i.lock().take();
            if t.is_none() {
                continue;
            }
            t.unwrap().err(CmdErr::Disowned)
        }

        let mut ql = self.cmd_queue.lock();
        while let Some(c) = ql.pop_front() {
            c.err(CmdErr::Disowned);
        }
    }

    /// Clears the interrupt status' specified in `int`.
    /// The caller should ensure that any interrupts to be cleared are handled properly.
    fn int_clear(&self, int: InterruptStatus) {
        self.port.lock().interrupt_status.clear(int);
    }

    async fn update(&self) {
        if self.handle_int() {
            self.refresh_exec().await
        }
    }
}

impl core::fmt::Debug for Port {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.port, f)
    }
}

impl core::fmt::Display for Port {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} - {}", self.info.pci_addr, self.index)
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

impl DevIdentity {
    /// Checks if the given args will exceed the last lba on the device.
    /// This fn does not check that the arguments given to it are sane
    ///
    /// # Panics
    ///
    /// This fn will panic if `lba + count` overflows. (Not that it should ever be allowed to)
    pub fn exceeds_dev(&self, lba: SectorAddress, count: SectorCount) -> bool {
        lba.raw() + count.count_ext() as u64 > self.lba_count
    }
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
    // contains whether this command used NQC. This is here exclusively for error checking
    nqc: atomic::Atomic<bool>,
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
    /// The command is waiting to be completed
    Waiting,
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
    /// Contains the error bits from the command.
    DevErr(u8),
    /// The system is no longer in communication with the target device.
    Disowned,
    /// The caller gave invalid arguments to the fn. The fn should document arguments which may
    /// cause this err.
    BadArgs,
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

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
pub struct SectorAddress(u64);

impl SectorAddress {
    pub fn new(addr: u64) -> Option<Self> {
        if addr < 1 << 48 {
            Some(Self(addr))
        } else {
            None
        }
    }

    fn raw(&self) -> u64 {
        self.0
    }
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
pub struct SectorCount(core::num::NonZeroU32);

impl SectorCount {
    pub fn new(count: u32) -> Option<Self> {
        match count {
            0 => None,
            n if n > (1 << 16) => None,
            n => Some(Self(core::num::NonZeroU32::new(n).unwrap())), // zero is checked above
        }
    }

    /// Returns the raw number of sectors for a 48 bit command.
    /// This fn should be preferred over [Self::count].
    fn count_ext(&self) -> u16 {
        let n = self.0.get();
        if n == 1 << 16 {
            0
        } else {
            n as u16
        }
    }

    /// Returns the raw sector count for 28 bit commands
    fn count(&self) -> Option<u8> {
        match self.0.get() {
            256 => Some(0),
            n if n > 256 => None,
            n => Some(n as u8),
        }
    }
}

struct PortErrChk {
    inner: spin::mutex::Mutex<PortErrChkInner>,
}

struct PortErrChkInner {
    waiting: bool,
    // Contains the active commands which may have triggered an error.
    err_cmd: u32,
    // Contains the command currently being checked.
    err_chk: Option<u8>,
}

impl PortErrChk {
    const fn new() -> Self {
        Self {
            inner: spin::Mutex::new(PortErrChkInner {
                waiting: false,
                err_cmd: 0,
                err_chk: None,
            }),
        }
    }

    /// Checks If the given args signal an error and attempts to determine which command triggered the error.
    /// If the erroneous command cannot be determined immediately enters a state for checking which
    /// command triggered the error.
    fn chk(
        &self,
        tfd: crate::hba::port_control::TaskFileData,
        lock: &CmdLock,
        ci: u32,
    ) -> Option<u8> {
        let mut l = self.inner.lock();
        if tfd.get_err() != 0 {
            for (i, s) in lock.cmd.iter().enumerate() {
                let bit = 1 << i;
                if s.load(atomic::Ordering::Relaxed) == CmdLockState::Running && (ci & bit == 0) {
                    l.err_cmd |= bit;
                }
            }

            if l.err_cmd.count_ones() == 1 {
                l.err_cmd = 0;
                Some(l.err_cmd.trailing_zeros() as u8)
            } else {
                l.err_chk = Some(l.err_cmd.trailing_zeros() as u8);
                None
            }
            // check if only one command is potential cause
        } else {
            None
        }
    }

    fn is_err(&self) -> bool {
        let l = self.inner.lock();
        l.err_chk.is_some() || l.waiting
    }

    /// Returns the next command to be checked for errors
    fn next(&self) -> u8 {
        let mut l = self.inner.lock();
        let ret = l.err_chk;
        l.err_chk = Some(l.err_cmd.trailing_zeros() as u8);

        // clears the bit being checked
        l.err_cmd ^= 1 << l.err_chk.unwrap();
        ret.unwrap()
    }
}
