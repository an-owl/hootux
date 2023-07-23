use crate::hba::command::{CommandHeader, CommandTableRaw};

/// This is used to store a table that is not used to send commands and has a constant location in
/// physical memory.
static DEAD_TABLE: DeadTable = DeadTable::new();

/// CmdList keeps an array of command tables which can be used to issue commands to the attached device.
/// Command tables are internally kept behind mutexes to prevent data races.
// the list must never be reordered this will cause a bad time.
pub(super) struct CmdList {
    info: super::HbaInfoRef,
    table_top: core::ptr::NonNull<[CommandHeader; 32]>,
    list: [Option<spin::Mutex<UnboundCommandTable>>; 32],
}

unsafe impl Send for CmdList {}
unsafe impl Sync for CmdList {}

impl CmdList {
    /// Allocates new command list with tables for each active command slot.
    pub(super) fn new(info: super::HbaInfoRef) -> Self {
        use core::alloc::Allocator;
        let alloc = hootux::allocator::alloc_interface::DmaAlloc::new(info.mem_region(), 128);
        let mut cmd_list = [const { None }; 32];

        let table_top = alloc
            .allocate(core::alloc::Layout::new::<[CommandHeader; 32]>())
            .expect("System ran out of memory")
            .cast::<[CommandHeader; 32]>();

        let mut headers = unsafe { &mut *table_top.as_ptr() }
            .each_mut()
            .map(|i| Some(i));

        for i in 0..info.queue_depth as usize {
            const DEF_TABLE_LEN: u16 = 64;
            let (table, addr) = CommandTableRaw::new(DEF_TABLE_LEN, info.mem_region());
            // SAFETY: the table has just been allocated with the given size
            unsafe {
                // all elements in the array are Some(_) at this point
                headers[i].as_mut().unwrap().set_table(addr, DEF_TABLE_LEN);
            }

            let nt = UnboundCommandTable {
                info: info.clone(),
                table_size: DEF_TABLE_LEN,
                parent: headers[i].take().unwrap(),
                table,
            };
            cmd_list[i] = Some(spin::Mutex::new(nt));
        }

        Self {
            info,
            table_top,
            list: cmd_list,
        }
    }

    /// Returns a reference to the command table if one exists.
    pub(super) fn table(&self, idx: u8) -> Option<CommandTable> {
        Some(CommandTable {
            table: self.list[idx as usize]
                .as_ref()?
                .try_lock()
                .expect("AHCI: Command table already locked"),
        })
    }

    /// Returns a pointer to the raw command table
    pub(super) fn table_addr(&self) -> *const [CommandHeader; 32] {
        self.table_top.as_ptr()
    }
}

impl Drop for CmdList {
    fn drop(&mut self) {
        use core::alloc::Allocator;

        let ptr = self.table_top.cast();
        let layout = core::alloc::Layout::new::<[CommandHeader; 32]>();
        unsafe {
            hootux::allocator::alloc_interface::DmaAlloc::new(self.info.mem_region(), 128)
                .deallocate(ptr, layout)
        }
    }
}

pub(super) struct CommandTable<'a> {
    table: spin::MutexGuard<'a, UnboundCommandTable>,
}

// Note - SAFETY: These are safe because the parent table is bounded by the lifetime of self
impl<'a> CommandTable<'a> {
    /// Builds the PRDT and sends the FIS to the device
    pub fn send_fis(
        &mut self,
        fis: crate::hba::command::frame_information_structure::RegisterHostToDevFis,
        buff: Option<*mut [u8]>,
    ) -> Result<(), CommandError> {
        // See Note above
        unsafe { self.table.send_fis(fis, buff) }
    }
}

/// This struct is a container for a command table. This struct is to enable control of the command
/// table such as relocating and resizing it. The actual size of the table is cached to prevent unnecessary shrinking of the table
///
/// The PRDT has a minimum len of 1 and a maximum of 65536. A table len of `0` will be interpreted in hardware as 65536.
///
/// The command table uses [hootux::allocator::alloc_interface::MmioAlloc] to prevent the struct
/// from being dropped in physical memory. However allocations must be performed using
/// [hootux::allocator::alloc_interface::DmaAlloc] because the physical memory structure cannot be fragmented.
///
/// # Safety
///
/// The methods for this struct are unsafe because they dereference a raw pointer owned by a
/// different struct. The caller must ensure that the parent table points to a [CommandHeader].
/// [CommandTable] is provided for this.
pub(crate) struct UnboundCommandTable {
    info: super::HbaInfoRef,
    table_size: u16,
    // this needs to be a raw pointer because this needs to be stored in `super::Port` which owns `self.parent`. This causes problems.
    // This can be removed from Self along with `info` and moved into `CommandTable` which can be constructed as needed.
    // The rest must be kept within `super::Port` for optimization reasons.
    parent: *mut CommandHeader,
    table: alloc::boxed::Box<CommandTableRaw, hootux::allocator::alloc_interface::MmioAlloc>,
}

type BoxedCommandTable =
    alloc::boxed::Box<CommandTableRaw, hootux::allocator::alloc_interface::MmioAlloc>;

impl UnboundCommandTable {
    /// Imports the new command table updating pointers and sizes.
    unsafe fn import_table(&mut self, table: BoxedCommandTable, phys_addr: u64) {
        let header = &mut *self.parent;
        // 1 is min size, this prevents the table from being accessed accidentally.
        header.physical_region_table_len.write(1);

        header.command_table_base_addr.set(phys_addr);

        self.table = table;
        let len = if self.table.len() == 0x1_0000 {
            0
        } else {
            self.table.len()
        };
        header.physical_region_table_len.write(len as u16);
    }

    /// Builds the PRDT for self updating the size if required. The caller should ensure that the
    /// buffer size is aligned to the devices LBA size falling to do this may cause errors.
    ///
    /// `region` must be aligned to `2` and must vbe fully mapped into memory or this will return [CommandError::BadBuffer]
    /// If the HBA only supports 32bit addressing then the buffer must be entirely mapped to 32bit
    /// address space otherwise this fn will return [CommandError::BadAddress]
    ///
    /// When this fn returns `Err(CommandError::BuffTooLong(n))` the PRDT has been built for `n` bytes.
    /// A second command will be required to fill the remaining buffer.
    // todo: region should ideally use a raw pointer. This is ok for not but watch "raw_slice_len" https://github.com/rust-lang/rust/issues/71146
    pub(crate) unsafe fn build_prdt(&mut self, region: &[u8]) -> Result<(), CommandError> {
        // increment: this is a separate definition because I may find a reason to change the increments later

        if region.len() % 2 != 0 {
            return Err(CommandError::BadBuffer);
        }

        let mut of = alloc::vec::Vec::new();
        let mut index = core::cell::Cell::new(0); // counts the number of table entries used only mutate in flush()

        let mut rem = region.len(); // who?
        let mut ptr = region.as_ptr();
        let mut blk: Option<(u64, u64)> = None;

        let mut flush = |start, len| {
            let entry = crate::hba::command::PhysicalRegionDescription::new(start, len)
                .expect("Failed to create physical region description");
            if index.get() > self.table_size {
                of.push(entry);
            } else {
                self.table[index.get()] = entry;
            }
            index.set(index.get() + 1);
        };

        // stores the return code. This is used over a `return Err(_)` because this fn needs to
        // finalize things regardless of the error
        let mut incomplete = Ok(());
        loop {
            // check that prdt space remains
            if index.get() == u16::MAX {
                incomplete = Err(CommandError::BuffTooLong(region.len() - rem));
                break;
            }

            let (len, new_blk) = Self::gen_blk(ptr, rem);
            let phys_addr =
                hootux::mem::mem_map::translate(ptr as usize).ok_or(CommandError::BadBuffer)?;

            // checks the address is 32bit if required
            if !self.info.is_64_bit && phys_addr & !0xffff_ffff != 0 {
                return Err(CommandError::BadAddress);
            }

            // if contiguous, append new blk
            //     if next blk will be too long flush now
            //         if table flush returns true return BuffTooLong
            // else flush block
            //     return BuffTooLong if flush returns true

            match blk {
                // block is contiguous
                Some((start, last)) if last + len as u64 == phys_addr => {
                    if ((phys_addr + len as u64) - start) > (1 << 21) {
                        // exceeds maximum block size
                        flush(start, (last - start) as u32);
                        blk = Some((phys_addr, phys_addr + len as u64));
                    } else {
                        blk = Some((blk.unwrap().0, phys_addr + len as u64));
                    }
                }

                // block is not contiguous
                Some((start, last)) if last + len as u64 != phys_addr => {
                    flush(start, ((start - last) / 2) as u32);
                    blk = Some((phys_addr, phys_addr + len as u64));
                }

                None => {
                    blk = Some((phys_addr, phys_addr + len as u64));
                }
                _ => unreachable!(),
            }

            if let Some(nb) = new_blk {
                ptr = nb;
            } else {
                if let Some((start, last)) = blk {
                    flush(start, (last - start) as u32);
                }
                break;
            }
            rem -= len;
        }

        // realloc table if necessary
        if of.len() != 0 {
            // in theory if this overflows the result will be 0. If its not then the las index has been incorrectly calculated
            // new size
            let ns = match self.table_size.overflowing_add(of.len() as u16) {
                (0, true) => 0u16,
                (0, false) => panic!(), // This should never happen, flush has incorrectly calculated last index
                (n, false) => n,
                (_, true) => panic!(), // see (0,false) arm
            };

            let (mut nt, addr) = CommandTableRaw::new(ns, self.info.mem_region());

            // moves new entries from old table
            for i in 0..self.table.len() {
                nt[i as u16] = self.table[i as u16];
            }

            // move entries from overflow
            for (c, i) in (self.table.len()..ns as usize).enumerate() {
                nt[c as u16] = of[i];
            }

            self.import_table(nt, addr);
        } else {
            self.set_len(index.get());
        }

        incomplete
    }

    /// Attempts to locate an aligned block form a pointer and a remainder.
    /// Returns the size of the block and address of the next block if any remains.
    fn gen_blk(mut ptr: *const u8, rem: usize) -> (usize, Option<*const u8>) {
        const INC: usize = hootux::mem::PAGE_SIZE;

        let addr = ptr as usize;

        // chk align
        let offset = addr & (INC - 1);
        return if offset != 0 {
            // not aligned to INC

            if rem <= (INC - offset) {
                // If block ends before next INC
                // (INC-offset) is size until next INC
                (rem, None)
            } else {
                (
                    INC - offset,
                    Some(unsafe { ptr.offset((INC - offset) as isize) }),
                )
            }
        } else {
            if INC < rem {
                // last block not aligned
                (rem, None)
            } else if rem == INC {
                // last block aligned
                (INC, None)
            } else {
                (INC, Some(unsafe { ptr.offset(INC as isize) }))
            }
        };
    }

    /// Sets the len of the PRDT within the command header.
    /// if `len` is greater than `self.table_size` this will cause the
    /// table to be reallocated.
    pub(crate) unsafe fn set_len(&mut self, len: u16) {
        if self.table_size < len {
            let (t, a) = CommandTableRaw::new(len, self.info.mem_region());
            self.import_table(t, a);
            self.table_size = len;
        } else {
            (*self.parent).physical_region_table_len.write(len);
        }
    }

    /// Builds the PRDT and sends the given fis to the device
    pub(super) unsafe fn send_fis(
        &mut self,
        cmd: crate::hba::command::frame_information_structure::RegisterHostToDevFis,
        buff: Option<*mut [u8]>,
    ) -> Result<(), CommandError> {
        if let Some(buff) = buff {
            self.build_prdt(unsafe { &*buff })?
        }
        (*self.parent).set_fis_len(
            (core::mem::size_of::<
                crate::hba::command::frame_information_structure::RegisterHostToDevFis,
            >() / 4)
                .try_into()
                .unwrap(),
        );

        self.table.command_fis.send_cmd(&cmd);

        let t: ata::command::AtaCommand = cmd.command.try_into().unwrap();
        // todo check if dev is ATAPI
        if !t.is_nqc() && self.table.len() == 0 {
            (*self.parent).set_prefetch(buff.is_some());
        }

        Ok(())
    }
}

impl Drop for UnboundCommandTable {
    fn drop(&mut self) {
        let p = unsafe { &mut *self.parent };
        p.physical_region_table_len.write(1);

        p.command_table_base_addr.set(DEAD_TABLE.get_addr());
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CommandError {
    /// Attempted to use a 64bit address on a device does not support 64bit addressing.
    BadAddress,
    /// Given buffer is too long, contained value contains the index of the first unused byte in the buffer.
    /// This indicates a partial success, but not all data has been processed.
    /// Functions that return this should detail how to handle this result
    BuffTooLong(usize),
    /// A buffer likely has some requirements to it to be usable, any functions that may return
    /// this should define a usable buffer.
    BadBuffer,
}

struct DeadTable {
    state: atomic::Atomic<DeadState>,
    table: core::cell::Cell<Option<&'static CommandTableRaw>>,
    phys_addr: core::cell::Cell<u64>,
}

unsafe impl Send for DeadTable {}
unsafe impl Sync for DeadTable {}

impl DeadTable {
    const fn new() -> Self {
        Self {
            state: atomic::Atomic::new(DeadState::UnInit),
            table: core::cell::Cell::new(None),
            phys_addr: core::cell::Cell::new(0),
        }
    }

    #[cold]
    fn init(&self) {
        use core::sync::atomic::Ordering;
        match self.state.compare_exchange_weak(
            DeadState::UnInit,
            DeadState::Initializing,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                // init
                // 1 is min size, just use mem32, the device initializing it may not be the only device using it.
                let (table, addr) =
                    CommandTableRaw::new(1, hootux::mem::buddy_frame_alloc::MemRegion::Mem32);
                let t = alloc::boxed::Box::leak(table);
                self.table.set(Some(t));
                self.phys_addr.set(addr);
                self.state.store(DeadState::Ready, Ordering::Release);
            }

            Err(_) => self.wait_for_ready(),
        }
    }

    #[cold]
    fn wait_for_ready(&self) {
        while self.state.load(atomic::Ordering::Acquire) != DeadState::Ready {
            core::hint::spin_loop();
        }
    }

    #[inline]
    /// Returns an address to a a table that is valid but cannot be used to send commands with a size of `1`.
    fn get_addr(&self) -> u64 {
        if self.state.load(atomic::Ordering::Acquire) == DeadState::Ready {
            self.phys_addr.take()
        } else {
            self.init();
            self.phys_addr.take()
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum DeadState {
    UnInit,
    Initializing,
    Ready,
}
