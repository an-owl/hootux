/// This module contains APIC register types and their implementations
///
/// Data stored within register types are stored as MaybeUninit<T> to prevent unwanted access.
/// It also helps to enforce read/write restrictions. All register data is initialized except
/// for [EOIRegister] which is Write only
pub mod registers{
    use core::fmt::{Debug, Formatter};
    use core::mem::MaybeUninit;
    use super::apic_types::*;



    pub trait LocalVectorEntry {

        fn get_reg(&self) -> &u32;

        fn get_reg_mut(&mut self) -> &mut u32;

        /// Sets the vector and delivery mode. these are done together because some vectors are
        /// invalid for some modes must have a vector number of 0. Check these modes in the intel
        /// software developers manual volume 3 chapter 10.5.1. for these modes vectors are
        /// automatically adjusted
        ///
        /// #Safety
        /// this function is unsafe because setting an invalid vector or mode may result in
        /// undefined behaviour. The programmer must ensure that the given vector is handled.
        ///
        /// #Panics
        /// This function will panic if `vector < 16` if a mode is use where the vector should be 0
        /// any vector within 16..=255 may be used and will be corrected internally to 0
        unsafe fn set_vector(&mut self, vector: u8, mode: InterruptDeliveryMode ){
            assert!((vector > 15), "Invalid interrupt vector");
            *self.get_reg_mut() &= !0x3ff;
            *self.get_reg_mut() |= mode.check_vector(vector) as u32;
            *self.get_reg_mut() |= (mode as u32) << 8
        }

        fn get_vector(&self) -> u8 {
            *self.get_reg() as u8
        }

        fn get_mode(&self) -> InterruptDeliveryMode {
            InterruptDeliveryMode::from(*self.get_reg())
        }

        /// Sets the interrupt maks to `mask` with the mask `true` the interrupt is disabled,
        /// and false it is enabled
        ///
        /// #Saftey
        /// This function is unsafe because if its interrupt vector is not set correctly it will
        /// result in a double fault.
        unsafe fn set_mask(&mut self, mask: bool){
            *self.get_reg_mut() &= (mask as u32) << 16;
        }

        fn get_mask (&self) -> bool {
            if *self.get_reg() & (1 << 16) > 0 {
                true
            } else { false }
        }

        fn get_status(&self) -> bool{
            return if self.get_reg() & (1 << 12) > 0 {
                true
            } else {
                false
            }
        }
    }


    /// Represents the LVT entry for controlling timer interrupts
    pub struct TimerIntVector{
        inner: MaybeUninit<u32>
    }

    impl LocalVectorEntry for TimerIntVector {
        fn get_reg(&self) -> &u32 {
            unsafe { self.inner.assume_init_ref() }
        }

        fn get_reg_mut(&mut self) -> &mut u32 {
            unsafe { self.inner.assume_init_mut() }
        }
    }

    impl Debug for TimerIntVector {
        fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
            let mut b = f.debug_struct("TimerVector");
            b.field("timer_mode",&self.get_timer_mode());
            b.field("mask", &self.get_mask());
            b.field("delivery_mode", &self.get_mode());
            b.field("vector", &self.get_vector());

            b.finish()
        }
    }

    impl TimerIntVector{
        const TIMER_MODE_MASK: u32 = 3 << 17;
        pub fn set_timer_mode(&mut self, mode: TimerMode) {
            unsafe {
                let data = self.inner.assume_init_mut();
                *data &= !Self::TIMER_MODE_MASK; // clears bits
                let new: u32 = mode.into();
                *data |= new; // sets bits
            }
        }

        pub fn get_timer_mode(&self) -> TimerMode {
            unsafe { TimerMode::from(*self.inner.assume_init_ref()) }
        }
    }

    /// Represents local interrupt LVT entries for controlling pins LINT0 and LINT1 on the cpu
    pub struct LocalInt{
        inner: MaybeUninit<u32>
    }

    impl LocalVectorEntry for LocalInt {
        fn get_reg(&self) -> &u32 {
            unsafe { self.inner.assume_init_ref() }
        }

        fn get_reg_mut(&mut self) -> &mut u32 {
            unsafe { self.inner.assume_init_mut() }
        }
    }

    impl Debug for LocalInt {
        fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
            let mut b = f.debug_struct("LocalInt");
            b.field("mask", &self.get_mask());
            b.field("trigger_mode",&self.get_trigger_mode());
            b.field("remote_irr", &self.get_interrupt_request());
            b.field("pin_polarity", &self.get_polarity());
            b.field("delivery_mode", &self.get_mode());
            b.field("vector", &self.get_vector());

            b.finish()
        }
    }

    impl LocalInt {

        /// Reads the physical pins current polarity.
        fn get_polarity(&self) -> bool {
            let reg = unsafe { self.inner.assume_init().clone() };
            if (reg & (1 << 13)) > 0 {
                true
            } else { false }
        }

        /// Reads the trigger mode of the interrupt handler
        /// See [Self::set_trigger_mode] for state identification
        pub fn get_trigger_mode(&self) -> bool {
            let reg = unsafe { self.inner.assume_init().clone() };
            if (reg & (1 << 15)) > 0 {
                true
            } else { false }
        }

        /// Sets the trigger mode for self where `true` is level sensitive and `false` is edge
        /// sensitive
        ///
        /// This function is unsafe because it is not available in most cases and may cause errors
        /// or undefined behaviour. To prevent errors or undefined behaviour the following rules
        /// should be followed
        /// - "Level sensitive" must always be set when the delivery mode is ExtInt.
        /// - "Edge sensitive" must always be set when the delivery mode is not Fixed or ExtInt.
        /// - "Level sensitive" may not be set for LINT1.
        pub unsafe fn set_trigger_mode(&mut self, state: bool) {
            *self.inner.assume_init_mut() |= (state as u32) << 15;
        }

        /// Gets Interrupt Request state for fixed mode which high `true` while an interrupt is
        /// triggered and is reset to `false` when EOI is asserted.
        pub fn get_interrupt_request(&self) -> bool {
            let reg = unsafe { self.inner.assume_init().clone() };
            if (reg & (1 << 14)) > 0 {
                true
            } else { false }
        }
    }

    /// Represents LVT entries for interrupts generated internally be the Local APIC
    pub struct InternalInt {
        inner: MaybeUninit<u32>
    }

    impl Debug for InternalInt {
        fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
            let mut b = f.debug_struct("InternalInt");
            b.field("mask", &self.get_mask());
            b.finish()
        }
    }

    impl LocalVectorEntry for InternalInt {
        fn get_reg(&self) -> &u32 {
            unsafe { self.inner.assume_init_ref() }
        }

        fn get_reg_mut(&mut self) -> &mut u32 {
            unsafe {self.inner.assume_init_mut() }
        }
    }

    pub struct ApicErrorInt {
        inner: MaybeUninit<u32>
    }

    impl Debug for ApicErrorInt {
        fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
            let mut b = f.debug_struct("ApicErrorInt");
            b.field("mask", &self.get_mask());
            b.field("vector", &self.get_vector());
            b.finish()
        }
    }

    impl LocalVectorEntry for ApicErrorInt {
        fn get_reg(&self) -> &u32 {
            unsafe { self.inner.assume_init_ref() }
        }

        fn get_reg_mut(&mut self) -> &mut u32 {
            unsafe { self.inner.assume_init_mut() }
        }

        /// ApicErrorInt does not take an [InterruptDeliveryMode] so this value is may be
        /// safely set to anything.
        /// Otherwise see the default impl for [LocalVectorEntry::set_vector] for details
        unsafe fn set_vector(&mut self, vector: u8, _mode: InterruptDeliveryMode) {
            assert!(vector > 15, "Invalid interrupt vector");
            *self.get_reg_mut() &= !0xff;
            *self.get_reg_mut() |= vector as u32;

        }
    }

    bitflags::bitflags! {
        pub struct SpuriousVector: u32{
            const APIC_ENABLE = 1 << 8;
            const FOCOUS_PROCESSOR_CHECKING = 1 << 9;
            const EOI_BROADCAST_SUPPRESSION = 1 << 12;
        }
    }

    impl SpuriousVector {
        pub fn set_vector(&mut self, vector: u8) {
            assert!(vector > 15, "Invalid interrupt vector");

            self.bits &= !0xff; // clear lower byte
            self.bits |= vector as u32;

        }
    }

    pub struct EoiRegister {
        inner: MaybeUninit<u32> // write only
    }

    impl EoiRegister {
        pub fn notify(&mut self) { // consider changing this sto &self
            self.inner.write(0);
        }
    }

    bitflags::bitflags! {
        pub struct ApicError: u32{
            const CHECKSUM_SEND_ERROR       = 1;
            const RECIEVE_CHECKSUM_ERROR    = 1 << 1;
            const SEND_ACCEPT_ERROR         = 1 << 2;
            const RECIEVE_ACCEPT_ERROR      = 1 << 3;
            const REDIRECTIBLE_IPI          = 1 << 4;
            const SEND_ILLIGAL_VECTOR       = 1 << 5;
            const RECIEVED_ILLIGAL_VECTOR   = 1 << 6;
            const ILLIGAL_REGISTER_ADDRESS  = 1 << 7;
        }
    }

    impl ApicError {
        pub fn clear(&mut self) {
            *self = Self::empty();
        }
    }
}

pub mod apic_types {
    #[repr(u8)]
    #[derive(Copy,Clone,Debug,PartialEq)]
    pub enum InterruptDeliveryMode{
        Fixed       = 0o00,
        Smi         = 0o02,
        Nmi         = 0o04,
        ExternalInt = 0o07,
        Init        = 0o05,
    }

    impl InterruptDeliveryMode {
        /// Some vectors are invalid for some delivery modes. This function will check a
        /// vector number against self and return an appropriate value
        pub fn check_vector(&self, vector: u8) -> u8 {
            match self {
                InterruptDeliveryMode::Fixed => vector,
                InterruptDeliveryMode::Smi => 0,
                InterruptDeliveryMode::Nmi => 0,
                InterruptDeliveryMode::ExternalInt => vector,
                InterruptDeliveryMode::Init => vector,
            }
        }
    }

    impl From<u32> for  InterruptDeliveryMode {
        /// This is for running on an entire **impl LocalVectorTableType and should not be called
        /// anywhere else doing so may cause a panic
        fn from(mut initial: u32) -> Self {
            initial >>= 8;
            initial &= 0x7;

            return match initial {
                0o00 => Self::Fixed,
                0o02 => Self::Smi,
                0o04 => Self::Nmi,
                0o07 => Self::ExternalInt,
                0o05 => Self::Init,
                e => panic!("unable to convert {} into InterruptDeliveryMode",e)
            }
        }
    }

    #[repr(u8)]
    #[derive(Copy,Clone,Debug)]
    pub enum TimerMode{
        OneShot = 0,
        Periodic,
        TscDeadline,
    }

    impl From<u32> for TimerMode{
        fn from(mut initial: u32) -> Self {
            initial >>= 17;
            initial &= 0x3;

            return match initial {
                0 => Self::OneShot,
                1 => Self::Periodic,
                2 => Self::TscDeadline,
                _ => panic!()
            }
        }
    }
    impl Into<u32> for TimerMode {
        fn into(self) -> u32 {
            (self as u32) << 17
        }
    }

    #[derive(Copy,Clone,Debug,PartialEq)]
    enum TriggerMode{
        EdgeSensitive,
        LevelSensitive
    }

    impl Into<bool> for TriggerMode {
        fn into(self) -> bool {
            return match self {
                TriggerMode::EdgeSensitive => false,
                TriggerMode::LevelSensitive => true,
            }
        }
    }

    impl Into<u32> for TriggerMode {
        fn into(self) -> u32 {

            let decoded: bool = self.into();
            (decoded as u32) << 15
        }
    }


    impl From<bool> for TriggerMode {
        fn from(input: bool) -> Self {
            return if input{
                Self::LevelSensitive
            } else {
                Self::EdgeSensitive
            }
        }
    }

    impl From<u32> for TriggerMode {
        fn from(input: u32) -> Self {
        Self::from((input >> 15 & (1)) > 0)
    }
    }


    #[repr(u32)]
    #[derive(PartialEq,Debug,Clone)]
    pub enum TimerDivisionMode{
        Divide2 = 0,
        Divide4 = 0o1,
        Divide8 = 0o2,
        Divide16 = 0o3,
        Divide32 = 0x10,
        Divide64 = 0o11,
        Divide128 = 0o12,
        Divide1 = 0o13, //why is this the highest?
    }
}