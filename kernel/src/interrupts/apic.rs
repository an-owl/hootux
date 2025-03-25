use crate::alloc_interface::MmioAlloc;
use crate::interrupts::apic::apic_structures::apic_types::TimerMode;
use crate::interrupts::apic::pub_apic::SysApic;
use crate::interrupts::apic::xapic::xApic;
use crate::util::KernelStatic;
use alloc::boxed::Box;
use apic_structures::registers::ApicError;
use modular_bitfield::BitfieldSpecifier;

pub mod apic_structures;
pub mod ioapic;
pub mod pub_apic;
pub mod xapic;

/// Contains the cpus local APIC. Uses mutex in case of mischievous r/w
#[thread_local]
pub(crate) static LOCAL_APIC: KernelStatic<Box<dyn Apic, crate::mem::allocator::GenericAlloc>> =
    KernelStatic::new();

static TIMER_IRQ: crate::util::Worm<super::InterruptIndex> = crate::util::Worm::new();

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

    /// Sends an IPI
    ///
    /// See ISDM v4 10.6 for info.
    ///
    /// # Safety
    ///
    /// This is unsafe because it causes the target(s) to take actions which may cause UB.
    unsafe fn send_ipi(
        &mut self,
        target: IpiTarget,
        int_type: InterruptType,
        vector: u8,
    ) -> Result<(), IpiError>;

    /// Waits until `timeout` for IPI to be received.
    /// Returns `false` if timeout is exceeded.
    fn block_ipi_delivered(&self, timeout: crate::task::util::Duration) -> bool;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IpiError {
    /// The LAPIC cannot target this APIC-ID.
    /// This occurs on xAPIC devices when a target greater than `255` is used.
    BadTarget,
    /// A non-zero vector was used and the delivery mode was not fixed.
    ///
    /// All non-Fixed modes must use vector 0 for forward compatibility.
    BadMode,
}

pub enum IpiTarget {
    /// 00: Targets the CPU  given in the tuple field.
    Other(crate::mp::CpuIndex),
    /// 01: Interrupts this CPU.
    ThisCpu,
    /// 10: Interrupts all CPUs,
    All,
    /// 11: Interrupts all CPUs but not the issuer.
    AllNotThisCpu,
}

#[derive(BitfieldSpecifier, Copy, Clone, Eq, PartialEq)]
#[bits = 3]
pub enum InterruptType {
    /// Raises an interrupt for the vector given in the vector field.
    Fixed = 0,
    /// Delivers the interrupt to the CPU with the lowest priority among the processors specified.
    /// This a broadcast target or a logical addressing mode.
    /// This priority is determined by the TPR register
    ///
    /// This is model specific, I can't find how to determine if this is supported
    LowestPriority,
    /// Raises a System Management Interrupt. This causes the CPU to switch to SMM and do things.
    /// SMM may do nothing, it may do something, whatever it does will be transparent to the OS.
    Smi,
    /// Raises an NMI, this is pretty self explanatory.
    NMI = 4,
    /// Signals INIT to the target.
    /// This should **never** be used with a broadcast or logical target, while it is *technically* allowed it can cause problems.
    ///
    /// This resets the target and places it into wait for SIPI mode.
    Init,
    /// Startup IPI.
    /// Causes the target to start executing code at the page address given in the vector field i.e 0xVV0_0000.
    /// If the target is not in a wait-for-SIPI state then this interrupt is ignored by the target
    ///
    /// See [InterruptType::Init] for info about using this with broadcast targets
    SIPI,
}

// todo make this configurable at build time
const TARGET_FREQ: u32 = 300;
const TARGET_PERIOD: u64 = (1000000000f64 * (1f64 / TARGET_FREQ as f64)) as u64; // no touch

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
pub fn cal_and_run(time: u32) {
    if !TIMER_IRQ.is_set() {
        // SAFETY: MP not initialized, this cannot cause race conditions
        unsafe {
            TIMER_IRQ.write(super::InterruptIndex::Generic(
                super::reserve_single(0).unwrap(),
            ));
        }
    }

    LOCAL_APIC
        .get()
        .begin_calibration(time, TIMER_IRQ.read().as_u8());

    unsafe {
        super::vector_tables::alloc_irq_special(TIMER_IRQ.as_u8(), timer_handler).expect("???");
        LOCAL_APIC.get().init_timer(TIMER_IRQ.as_u8(), false);
    };
}

/// Attempts to start the timer on the Local APIC using the current timer IRQ and tick rate.
/// This fn will fail if a timer has not already been configured.
///
/// # Safety
///
/// This fn **is** safe because it required a pre-existing configuration.
pub(crate) fn try_start_timer_residual() -> Result<(), ()> {
    unsafe {
        LOCAL_APIC.get().init_timer(TIMER_IRQ.as_u8(), false);
        LOCAL_APIC
            .get()
            .set_timer(TimerMode::Periodic, CALI.ok_or(())? as u32)
    }
    Ok(())
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
}

/// Declares End Of Interrupt on apic devices, without potentially causing a deadlock.
///
/// # Safety
///
/// This must not be called outside of an interrupt handler
#[inline]
pub(crate) unsafe fn apic_eoi() {
    unsafe { LOCAL_APIC.force_get_mut().declare_eoi() }
}

/// Returns an interface for the apic
pub fn get_apic() -> SysApic {
    SysApic::new()
}

#[derive(modular_bitfield::BitfieldSpecifier)]
#[bits = 2]
enum DestinationShorthand {
    NoShorthand,
    ThisCpu,
    All,
    AllNotSelf,
}
