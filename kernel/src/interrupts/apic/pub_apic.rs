use super::{InterruptType, IpiError, IpiTarget, LOCAL_APIC};
use crate::interrupts::apic::apic_structures::registers::ApicError;
use crate::time::{Duration, Timer, TimerError, TimerMode, TimerResult};

/// This struct is for used to provide a public interface for the local apic~
pub struct SysApic {
    _h: Hidden,
}

/// Does Nothing, prevents instancing `SysApic` using `SysApic{}`
struct Hidden;

impl SysApic {
    pub(super) const fn new() -> Self {
        SysApic { _h: Hidden }
    }
}

impl Timer for SysApic {
    fn get_division_mode(&self) -> u32 {
        LOCAL_APIC.get().get_division_mode()
    }

    fn set_division_mode(&mut self, div: u32) -> TimerResult {
        LOCAL_APIC.get().set_division_mode(div)
    }

    fn set_clock_count(&mut self, count: u64, mode: TimerMode) -> TimerResult {
        LOCAL_APIC.get().set_clock_count(count, mode)
    }

    fn get_initial_clock(&self) -> Result<u64, TimerError> {
        LOCAL_APIC.get().get_initial_clock()
    }
}

impl super::Apic for SysApic {
    unsafe fn set_enable(&mut self, enable: bool) {
        LOCAL_APIC.get().set_enable(enable)
    }

    unsafe fn init_err(&mut self, vector: u8, mask: bool) {
        LOCAL_APIC.get().init_err(vector, mask)
    }

    unsafe fn init_timer(&mut self, vector: u8, mask: bool) {
        LOCAL_APIC.get().init_timer(vector, mask)
    }

    unsafe fn set_timer(
        &mut self,
        mode: crate::interrupts::apic::apic_structures::apic_types::TimerMode,
        time: u32,
    ) {
        LOCAL_APIC.get().set_timer(mode, time)
    }

    /// Do not call this fn it will panic. `declare_eoi` is not available via this interface. You may want to use [super::declare_eoi]
    fn declare_eoi(&mut self) {
        panic!("Tried to declare EOI via SysApic")
    }

    /// This fn is intended to only be used for interrupts and will panic if called
    fn get_err(&self) -> ApicError {
        panic!("Tried to call `get_err` via SysApic")
    }

    fn begin_calibration(&mut self, test_time: u32, vec: u8) {
        LOCAL_APIC.get().begin_calibration(test_time, vec)
    }

    /// Allowed but not recommended, use [crate::who_am_i] instead.
    fn get_id(&self) -> u32 {
        LOCAL_APIC.get().get_id()
    }

    unsafe fn send_ipi(&mut self, target: IpiTarget, int_type: InterruptType, vector: u8) -> Result<(), IpiError> {
        LOCAL_APIC.get().send_ipi(target,int_type,vector)
    }

    fn block_ipi_delivered(&self, timeout: Duration) -> bool {
        LOCAL_APIC.get().block_ipi_delivered(timeout)
    }
}
