bitflags::bitflags! {
    #[repr(transparent)]
    pub struct CommandRegister: u16 {
        /// (RW) When enabled allows device to respond to IO access
        // todo which access?
        // todo respond how?
        const IO_SPACE = 1;
        /// (RW) When enabled allows device to respond to memory assesses
        const MEMORY_SPACE = 1 << 1;
        /// (RW) When enabled device can act as a bus master and generate device accesses
        /// When disabled device cannot generate PCI accesses
        // todo what  does this mean?
        const BUS_MASTER = 1 << 2;
        /// (RO) Device can monitor special cycles
        // todo what are special cycles?
        // todo why?
        const SPECIAL_CYCLES = 1 << 3;
        /// (RO) Device can generate memory read and invalidate command otherwise Memory Write command
        /// must be used
        // todo what memory?
        // todo why?
        const MEMORY_WRITE_AND_INVALIDATE_ENABLE = 1 << 4;
        /// (RO) Device will snoop palette data otherwise regular writes must be used
        // todo snoop from where?
        const VGA_PALLETE_SNOOP = 1 << 5;
        /// (RW) Device will take normal action when a parity error is detected.
        /// If Disabled this device will set [StatusRegister::DETECTED_PARITY_ERR] and will not assert PERR# pin
        // todo what is normal action?
        const PARITY_ERROR_RESPONSE = 1 << 6;
        /// (RW) Enables SERR driver.
        // todo what triggers SERR
        // todo What is the proper response
        const SERR_ENABLE = 1 << 8;
        /// (RO) Device can generate fast back-to-back transactions otherwise fast back-to-back
        /// transactions are only enabled for the same agent
        // todo why does this need to be known?
        // todo does any action need to be taken?
        const FAST_BTB_ENABLE =  1 << 9;
        /// (RW) Disables INTn# from this device
        const INTERRUPT_DISABLE = 1 << 10;
    }

    /// The Status Register outputs the current status of the device.
    ///
    /// fields marked (RO) are Read-Only. Fields marked as (RW) can only be enabled by the device
    /// and can be cleared by writing 1.
    ///
    /// DEVSEL_TIMING bits 11-10 may be fast medium or slow. only medium or slow can be represented
    /// in bitflags if these flags are absent the speed is fast.
    #[repr(transparent)]
    pub struct StatusRegister: u16 {
        /// (RO) Interrupt Pending
        const INTERRUPT_STATUS = 1 << 3;
        /// (RO)Capabilities list can be found at offset `0x34`
        const CAPABILITES_LIST = 1 << 4;
        /// (RO) This device is capable of operating at 66MHz
        const FFQ66_MHZ_CAPABLE = 1 << 5;
        /// (RW) Device is capable of fast back-to-back transactions to different agents
        const FAST_BTB_CAPABLE = 1 << 7;
        /// The bus agent has asserted PERR# on read or observed a PERR# on write.
        /// The agent that enabled the bit acted as the bus master for the operation This will also set [CommandRegister::PARITY_ERROR_RESPONSE]
        const MASTER_DATA_PARITY_ERR =  1 << 8;
        /// (RO) Device Select timing (bits 10-11)
        // todo Device Select is a guess
        const DEVSEL_MEDIUM = 1 << 10;
        const DEVSEL_SLOW = 2 << 10;
        /// Set when a target terminates a transaction with Target-Abort
        // todo take action?
        // todo why will the target do this?
        const SIGNALLED_TARGET_ABORT = 1 << 11;
        /// Set by a bus master whenever its transaction is terminated with Target-Abort
        // todo only expected on bus masters? os does the master set this on the target?
        const RECIEVED_TARGET_ABORT = 1 << 12;
        /// Set by a master device when its transaction (except special cycles) are terminated using Master-Abort
        // todo can only the master signal master abort?
        // todo which device is this set on master or target
        const RECIEVED_MASTER_ABORT = 1 << 13;
        /// Set when the device asserts SERR#
        // todo what causes SERR#? What is the proper response?
        const SIGNALLED_SYSTEM_ERR = 1 << 14;
        /// Set when this device detects a parity error even when parity error handling is disabled
        // todo what actions should be taken?
        const DETECTED_PARITY_ERR = 1 << 15;
    }


    pub struct SelfTestRegister: u8 {
        /// Indicates whether the device can run a self test
        const CAPABLE = 1 << 7;
        /// Run bit. Set this bit to start a self test, This flag is unset when the test completes
        /// If this flag is active for more than 2 seconds the device can be considered failed.
        const TEST_RUN = 1 << 6;
    }
}

impl SelfTestRegister {
    /// Starts a Self test. If the device the `TEST_RUN` bit is not set to `0` within 2 seconds the
    /// device is considered failed. TRReturns `Err(())` if BIST is not available.
    pub fn run_test(&mut self) -> Result<(), ()> {
        // This is safe because self is read by reference not raw ptr
        let mut t = unsafe { core::ptr::read_volatile(self) };
        if t.contains(Self::CAPABLE) {
            t.set(Self::TEST_RUN, true);
            unsafe { core::ptr::write_volatile(self, t) }
            Ok(())
        } else {
            Err(())
        }
    }
    /// Gets the error code of the last test. Returns None if a test is currently running. Returns
    /// the error code in a `Result<u8,u8>`. `Ok()` Indicates a return code of `0`.
    pub fn get_err(&self) -> Option<Result<u8, u8>> {
        let t = unsafe { core::ptr::read_volatile(self) };
        if t.contains(Self::TEST_RUN) {
            return None;
        } else {
            match t.bits & 7 {
                0 => Some(Ok(0)),
                e => Some(Err(e)),
            }
        }
    }
}

#[repr(u8)]
#[allow(dead_code)]
// This enum is cast to but never constructed (yet)
pub enum InterruptPin {
    Disabled,
    IntA,
    IntB,
    IntC,
    IntD,
}

#[repr(transparent)]
pub struct HeaderTypeRegister {
    data: u8,
}

impl HeaderTypeRegister {
    pub fn get_type(&self) -> HeaderType {
        let data = unsafe { core::ptr::read_volatile(&self.data) };
        data.into()
    }

    pub fn is_multiple_functions(&self) -> bool {
        let data = unsafe { core::ptr::read_volatile(&self.data) };
        if data & 0x80 != 0 {
            true
        } else {
            false
        }
    }
}

#[repr(u8)]
#[derive(Eq, PartialEq, Copy, Clone, Debug)]
pub enum HeaderType {
    Generic = 0,
    Bridge,
    CardBusBridge,
}

impl From<u8> for HeaderType {
    fn from(value: u8) -> Self {
        let masked = value & 3;
        match masked {
            0 => Self::Generic,
            1 => Self::Bridge,
            2 => Self::CardBusBridge,
            _ => panic!("Cannot convert {} into PciHeaderType", value),
        }
    }
}

impl Into<u8> for HeaderType {
    fn into(self) -> u8 {
        self as u8
    }
}

impl From<u16> for CommandRegister {
    fn from(value: u16) -> Self {
        Self { bits: value }
    }
}

pub struct BaseAddressRegister {
    data: u32,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum BarType {
    DwordIO,
    Dword(bool),
    Qword(bool),
}

impl BarType {
    pub fn alignment(&self) -> usize {
        match self {
            BarType::DwordIO => 4,
            _ => 16,
        }
    }
}

impl BaseAddressRegister {
    pub fn bar_type(&self) -> BarType {
        // todo ensure this is non-volatile

        Self::get_type(self.data)
    }

    fn get_type(data: u32) -> BarType {
        if data & 1 != 0 {
            return BarType::DwordIO;
        }

        let len = (data >> 1) & 0x3;

        let prefetch = if ((data >> 3) & 1) != 0 { true } else { false };

        match len {
            0 => BarType::Dword(prefetch),
            2 => BarType::Qword(prefetch),
            _ => unreachable!(),
        }
    }

    /// Writes the given physical address to the BAR truncating the address to the correct alignment
    /// Returns the value read back from the register
    ///
    /// # Safety
    ///
    /// The caller must ensure the given address is a valid physical address and that the register is not 64bits long
    #[inline]
    pub unsafe fn write(&mut self, addr: u32) -> u32 {
        unsafe { core::ptr::write_volatile(&mut self.data, addr) }
        self.read()
    }

    /// Reads the physical address in the register.
    pub fn read(&self) -> u32 {
        let data = unsafe { core::ptr::read_volatile(&self.data) };
        return if data & 1 == 0 {
            data & (!0xf)
        } else {
            data & (!3)
        };
    }

    /// Gets the alignment of the register. The alignemtn of the register can also be used ta the
    /// size of the  register.
    /// The returned value can be assumed to be a power of two.
    ///
    /// # Safety
    ///
    /// This fn requires writing to the BAR so it should not be called while the device is in use.
    pub unsafe fn alignment(&mut self) -> u32 {
        let cache = self.read();
        self.write(u32::MAX);

        let align = self.read();

        let bt = self.bar_type();
        self.write(cache);

        if let BarType::DwordIO = bt {
            (!(align & !3)).wrapping_add(1)
        } else {
            (!(align & !0xf)).wrapping_add(1)
        }
    }
}

pub struct BarRegisterLong {
    inner: u64,
}

impl BarRegisterLong {
    /// Reads the register masking the configuration bits
    pub fn read(&self) -> u64 {
        let d = unsafe { core::ptr::read_volatile(&self.inner) };
        d & (!0xf)
    }

    /// Writes the given physical address to the BAR truncating the address to the correct alignment
    /// Returns the value read back from the register
    ///
    /// # Safety
    ///
    /// The caller must ensure the given address is a valid physical address and that the register is not 64bits long
    pub unsafe fn write(&mut self, data: u64) -> u64 {
        unsafe { core::ptr::write_volatile(&mut self.inner, data) };
        self.read()
    }

    /// Gets the alignment of the register. The alignemtn of the register can also be used ta the
    /// size of the  register.
    /// The returned value can be assumed to be a power of two.
    ///
    /// # Safety
    ///
    /// This fn requires writing to the BAR so it should not be called while the device is in use.
    pub unsafe fn alignment(&mut self) -> u64 {
        let cache = self.read();
        self.write(u64::MAX);

        let align = (!(self.read() & (!0xf))) + 1;
        self.write(cache);
        align
    }
}
