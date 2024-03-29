use crate::alloc_interface::MmioAlloc;
use crate::interrupts::apic::apic_structures::apic_types::TimerMode;
use crate::interrupts::apic::pub_apic::SysApic;
use crate::interrupts::apic::xapic::xApic;
use crate::kernel_structures::KernelStatic;
use alloc::boxed::Box;
use apic_structures::registers::ApicError;

pub mod apic_structures;
pub mod pub_apic;
pub mod xapic;

/// Contains the cpus local APIC. Uses mutex in case of mischievous r/w
#[thread_local]
pub(crate) static LOCAL_APIC: KernelStatic<Box<dyn Apic, crate::mem::allocator::GenericAlloc>> =
    KernelStatic::new();

/// Trait for control over the Local Advanced Programmable Interrupt Controller
///
/// #LVT Entry
/// These registers contain interrupt configuration information. All registers contain an 8bit
/// vector and a mask bit. The vector may not be less than 16, however it should not be lower than 32
/// because vectors 0..32 are used for exception handling and may cause unwanted behaviour.
/// When set the mask bit will prevent an interrupt from being generated by the interrupt controller
pub trait Apic: crate::time::Timer {
    unsafe fn set_enable(&mut self, enable: bool);

    unsafe fn init_err(&mut self, vector: u8, mask: bool);

    unsafe fn init_timer(&mut self, vector: u8, mask: bool);

    unsafe fn set_timer(&mut self, mode: TimerMode, time: u32);

    /// Declares that the current interrupt has been handled.
    /// this should be the last thing called before the end of an interrupt handler.
    fn declare_eoi(&mut self);

    /// Gets the current error and clears the error register.
    fn get_err(&self) -> ApicError;

    /// Calibrates the local APIC timer to [TARGET_FREQ].
    /// `test_time` is the time used for the clock until the test is complete.
    ///
    /// Before this fn returns it will mask the timer. This will need to be re-enabled after
    /// changing the interrupt handler to a permanent one.
    ///
    /// #Panics
    ///
    /// Implementations should panic if `super::IHC\[vec\]` is already occupied
    fn begin_calibration(&mut self, test_time: u32, vec: u8);

    fn get_id(&self) -> u32;
}

const TARGET_FREQ: u32 = 300;
const TARGET_PERIOD: u64 = (1000000000f64 * (1 as f64 / TARGET_FREQ as f64)) as u64; // no touch

//#[thread_local]
static mut CALI: Option<u64> = None;

/// Interrupt handler used for calibrating the local APIC timer.
fn handle_timer_and_calibrate() {
    unsafe {
        CALI = Some(crate::time::get_sys_time());
        apic_eoi();
    }
}

/// Default timer handler. Updates system time then exits.
fn timer_handler() {
    crate::time::update_timer();
    crate::task::util::check_slp();
    unsafe { apic_eoi() };
}

/// Calibrates local APIC and sets interrupt handler.
/// This is temporary and required because of the privacy of [crate::kernel_statics]
pub fn cal_and_run(time: u32, vec: u8) {
    LOCAL_APIC.get().begin_calibration(time, vec);

    unsafe {
        super::vector_tables::alloc_irq_special(vec, timer_handler).expect("???");
        LOCAL_APIC.get().init_timer(vec, false);
    };
}

/// Determines type of apic and loads it into `LOCAL_APIC`
pub fn load_apic() {
    use core::alloc::Allocator;
    use x86_msr::Msr;
    let t = unsafe { x86_msr::architecture::ApicBase::read() };
    let addr = x86_64::PhysAddr::new(t.get_apic_base_addr());
    let apic;
    if (raw_cpuid::cpuid!(1).ecx >> 21) & 1 > 0 {
        // todo implement x2apic
        let alloc = unsafe { MmioAlloc::new_from_phys_addr(addr) };
        let mut sec = alloc
            .allocate(core::alloc::Layout::new::<xApic>())
            .expect("not enough memory")
            .cast::<xApic>();
        apic = unsafe { Box::from_raw_in(sec.as_mut(), alloc.into()) };
    } else {
        let alloc = unsafe { MmioAlloc::new_from_phys_addr(addr) };
        let mut sec = alloc
            .allocate(core::alloc::Layout::new::<xApic>())
            .expect("not enough memory")
            .cast::<xApic>();
        apic = unsafe { Box::from_raw_in(sec.as_mut(), alloc.into()) };
    }

    LOCAL_APIC.init(apic);
    crate::WHO_AM_I.init(LOCAL_APIC.get().get_id());
}

/// Declares End Of Interrupt on apic devices, without potentially causing a deadlock.
///
/// # Safety
///
/// This must not be called outside of an interrupt handler
#[inline]
pub(crate) unsafe fn apic_eoi() {
    LOCAL_APIC.force_get_mut().declare_eoi()
}

/// Returns an interface for the apic
pub fn get_apic() -> SysApic {
    SysApic::new()
}
