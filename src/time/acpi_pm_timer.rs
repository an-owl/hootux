use crate::system::acpi::data_access::{DataAccessType, DataSize};
use crate::time::TimeKeeper;

const NSEC_PER_CLOCK: f64 = 279.365114840015;



/// Acpi timer is provided by firmware and does **not** support the [super::Timer] trait.
/// This is because the ACPI timer only interrupts when the counter overflows
///
/// AcpiTimer **must** be checked at least every 4.6 while used as system timer to prevent an
/// unhandled overflow. Normal arithmetic will stop working if the clock counter exceeds the last
/// checked value.
// initialize after MmioAlloc
#[derive(Debug)]
pub struct AcpiTimer{
    timer: DataAccessType,
    supports_32bit: bool,
}

impl AcpiTimer {
    pub fn locate(timer_info: acpi::platform::PmTimer) -> Self {
        let mut access = DataAccessType::from(timer_info.base);
        if !access.is_size_defined() {
            unsafe { access.define_size(DataSize::DWord) } // size is always 32bit*
        }

        Self{
            timer: access,
            supports_32bit: timer_info.supports_32bit,
        }
    }
}

impl TimeKeeper for AcpiTimer {

    #[optimize(speed)]
    fn time_since(&self, old_time: u64) -> (u64,u64) {
        // todo speed this up
        let time: u32 = self.timer.read().try_into().unwrap(); // will never panic unless acpi spec changes
        let (mut diff, underflow) = time.overflowing_sub(old_time as u32);

        if underflow && !self.supports_32bit {
            diff &= 0xffffff; // sets top byte to 0
        }

        ((diff as f64 * NSEC_PER_CLOCK) as u64, time as u64)
    }
}