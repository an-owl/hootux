#![allow(unused_parens)] // due to modular_bitfield
use super::apic_structures::{apic_types::*, registers::*};
use super::{Apic, InterruptType, IpiTarget};
use crate::device_check::{DeviceCheck, MaybeExists};
use crate::time::{Duration, Timer, TimerError, TimerResult};
use core::fmt::{Debug, Formatter};
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use x86_msr::architecture::ApicBaseData;

#[allow(non_camel_case_types)]
#[repr(C, align(4096))]
/// Struct representing the cpu's Local xAPIC
/// This does not come with a constructor because it should already be resident within memory
/*
 * i cannot figure out if arbitration_priority or remote_read is present. The manual says its not
 * present on pentium 4 class cpus. but then directs you to finding Nehalem cpus which is it intel?
 * either way they're present but inaccessible until i figure it out
 */

pub struct xApic {
    _reserved0: [ApicReservedRegister; 2],
    id_register: AlignedRegister<u32>,
    version_register: AlignedRegister<u32>,
    _reserved1: [ApicReservedRegister; 4],

    task_priority: AlignedRegister<u32>,
    arbitration_priority: MaybeExists<AlignedRegister<WrappedDword>, NonExistentIGuess>, // i just cannot figure where these are available
    processor_priority: AlignedRegister<u32>,
    eoi_reg: EoiRegister,

    remote_read: MaybeExists<AlignedRegister<u32>, NonExistentIGuess>, // see arbitration_priority

    logical_destination: AlignedRegister<u32>,
    destination_format: AlignedRegister<u32>,

    spurious_interrupt_vector: AlignedRegister<SpuriousVector>,

    in_service: [AlignedRegister<u32>; 8],
    trigger_mode: [AlignedRegister<u32>; 8],
    interrupt_request: [AlignedRegister<u32>; 8],

    error_status: AlignedRegister<ApicError>,

    _reserved2: [ApicReservedRegister; 6],

    cmc_vector: AlignedRegister<InternalInt>,

    interrupt_command_register: (AlignedRegister<u32>, AlignedRegister<u32>),

    timer_vector: AlignedRegister<TimerIntVector>,
    thermal_sensor_vector: MaybeExists<AlignedRegister<InternalInt>, ThermalVectorCheck>, // not available on all processors
    perf_mon_counter_vector: MaybeExists<AlignedRegister<InternalInt>, PerfMonCheck>, // not available on all processors
    local_interrupt_0_vector: AlignedRegister<LocalInt>,
    local_interrupt_1_vector: AlignedRegister<LocalInt>,
    error_interrupt_vector: AlignedRegister<ApicErrorInt>,

    initial_timer_count: AlignedRegister<u32>,
    current_timer_count: AlignedRegister<u32>,
    _reserved: [ApicReservedRegister; 4],
    divide_configuration_register: AlignedRegister<TimerDivisionMode>,

    _reserved4: ApicReservedRegister, // offset should be 0x3f0
}

impl Apic for xApic {
    unsafe fn set_enable(&mut self, enable: bool) {
        self.spurious_interrupt_vector
            .data
            .set(SpuriousVector::APIC_ENABLE, enable);
    }

    unsafe fn init_err(&mut self, vector: u8, mask: bool) {
        self.error_status.data = ApicError::empty();
        unsafe {
            self.error_interrupt_vector
                .data
                .set_vector(vector, InterruptDeliveryMode::Fixed);
            self.error_interrupt_vector.data.set_mask(mask);
        }
    }

    unsafe fn init_timer(&mut self, vector: u8, mask: bool) {
        unsafe {
            self.timer_vector
                .data
                .set_vector(vector, InterruptDeliveryMode::Fixed);
            self.timer_vector.data.set_mask(mask);
        }
        self.divide_configuration_register.data = TimerDivisionMode::Divide1;
    }

    unsafe fn set_timer(&mut self, mode: TimerMode, time: u32) {
        self.timer_vector.data.set_timer_mode(mode);
        self.initial_timer_count.data = time;
    }

    fn declare_eoi(&mut self) {
        self.eoi_reg.notify();
    }

    fn get_err(&self) -> ApicError {
        self.error_status.data.clone()
    }

    fn begin_calibration(&mut self, test_time: u32, vec: u8) {
        unsafe {
            super::super::vector_tables::alloc_irq_special(vec, super::handle_timer_and_calibrate)
                .expect("Vector already occupied");

            self.init_timer(vec, false);
            self.set_timer(TimerMode::Periodic, test_time);
        }

        let initial_time;
        let duration;

        // compiler wont like this. Optimizations might cause bugs
        // this needs to be done twice because the first clock is always very slow
        loop {
            if let Some(_) = unsafe { super::CALI } {
                initial_time = crate::time::get_sys_time();
                unsafe {
                    super::CALI = None;
                }
                break;
            }
            x86_64::instructions::hlt()
        }

        loop {
            if let Some(new) = unsafe { super::CALI } {
                duration = new - initial_time;
                break;
            }
            x86_64::instructions::hlt()
        }

        // SAFETY: This is safe because all given vectors are handled

        let ratio = super::TARGET_PERIOD as f32 / duration as f32;
        let out = ratio * test_time as f32; // Uses fp for precision. shouldn't have to big of an effect on performance

        let (time, divide) = TimerDivisionMode::best_try_divide(out as u64);
        self.divide_configuration_register.data = divide;

        // SAFETY: init_timer(_,true) is safe to use and makes th following fn's to use
        unsafe {
            self.init_timer(vec, true);
            self.set_timer(TimerMode::Periodic, time);
            // disable all the stuff this enabled
            super::super::vector_tables::IHR.unset(vec);
        }
    }

    fn get_id(&self) -> u32 {
        let mut id = self.id_register.data;
        id >>= 24;
        id &= !255; // clear reserved bits
        id
    }

    unsafe fn send_ipi(
        &mut self,
        target: IpiTarget,
        int_type: InterruptType,
        vector: u8,
    ) -> Result<(), super::IpiError> {
        let (dst, short) = match target {
            IpiTarget::Other(v) => (v, super::DestinationShorthand::NoShorthand),
            IpiTarget::ThisCpu => (0, super::DestinationShorthand::ThisCpu),
            IpiTarget::All => (0, super::DestinationShorthand::All),
            IpiTarget::AllNotThisCpu => (0, super::DestinationShorthand::AllNotSelf),
        };

        if ((int_type != InterruptType::Fixed) && (int_type != InterruptType::SIPI)) && vector != 0
        {
            return Err(super::IpiError::BadMode);
        }

        let mut icrl = InterruptCommandRegisterLow::new();
        icrl.set_vector(vector);
        icrl.set_delivery_mode(int_type);
        icrl.set_polarity(true);
        icrl.set_dest_shorthand(short);

        let mut icrh = InterruptCommandRegisterHigh::new();
        icrh.set_destination(dst.try_into().map_err(|_| super::IpiError::BadTarget)?);

        while InterruptCommandRegisterLow::from_bytes(
            self.interrupt_command_register.0.data.to_le_bytes(),
        )
        .delivery_status()
        {
            // todo is this hint necessary?
            core::hint::spin_loop();
        }
        core::hint::black_box(&self.interrupt_command_register);
        self.interrupt_command_register.1.data = icrh.into();
        core::hint::black_box(&self.interrupt_command_register);
        core::sync::atomic::compiler_fence(atomic::Ordering::Acquire);
        self.interrupt_command_register.0.data = icrl.into();
        core::hint::black_box(&self.interrupt_command_register);
        Ok(())
    }

    fn block_ipi_delivered(&self, timeout: Duration) -> bool {
        let wakeup_time: crate::time::AbsoluteTime = timeout.into();
        while !wakeup_time.is_future() {
            if !InterruptCommandRegisterLow::from(self.interrupt_command_register.0.data)
                .delivery_status()
            {
                return true;
            }
            core::hint::spin_loop(); // emits `pause` instruction
        }
        !InterruptCommandRegisterLow::from(self.interrupt_command_register.0.data).delivery_status()
    }
}

impl xApic {
    pub fn fetch_addr() -> ApicBaseData {
        unsafe {
            use x86_msr::Msr;
            x86_msr::architecture::ApicBase::read()
        }
    }

    pub fn gen_err(&mut self) {
        for i in &mut self._reserved {
            i._space.write(20);
        }
    }

    pub fn clear_err(&mut self) {
        self.error_status.data = ApicError::empty();
    }

    pub fn get_err(&self) -> ApicError {
        self.error_status.data
    }

    pub fn get_time(&self) -> u32 {
        self.current_timer_count.data
    }

    pub fn set_spurious_vec(&mut self, vector: u8) {
        self.spurious_interrupt_vector.data.set_vector(vector);
    }
}

#[repr(C, align(16))]
/// This struct is for interacting with apic registers. All registers are 32bits aligned to 128bits
/// This ensures the correct read/write operations are used while maintaining alignment
///
/// This is also used to enable simple forward compatibility with the x2apic
struct AlignedRegister<T> {
    data: T,
}

impl<T: Debug> Debug for AlignedRegister<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.data)
    }
}

#[derive(PartialEq, PartialOrd, Clone)]
/// Some APIC registers can be interpreted as a normal u32 this struct provides this
struct WrappedDword(u32);

impl Deref for WrappedDword {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for WrappedDword {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl core::fmt::Display for WrappedDword {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Debug for WrappedDword {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl Debug for xApic {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut builder = f.debug_struct("xApic");
        builder.field("id", &self.id_register.data.clone());
        builder.field("version", &self.version_register.data.clone());
        builder.field("task_priority", &self.task_priority.data.clone());
        builder.field("arbitration_priority", &self.arbitration_priority);
        builder.field("processor_priority", &self.processor_priority.data.clone());
        builder.field("remote_read", &self.remote_read);
        builder.field(
            "logical_destination",
            &self.logical_destination.data.clone(),
        );
        builder.field("destination_format", &self.destination_format.data.clone());
        builder.field(
            "spurious_interrupt_vector",
            &self.spurious_interrupt_vector.data.clone(),
        );
        builder.field("in_service", &self.in_service);
        builder.field("trigger_mode", &self.trigger_mode);
        builder.field("interrupt_request", &self.interrupt_request);
        builder.field("error_status", &self.error_status.data.clone());
        builder.field("cmc_vector", &self.cmc_vector.data.get_reg());

        builder.field("timer_vector", &self.timer_vector);
        builder.field("thermal_sensor_vector", &self.thermal_sensor_vector);
        builder.field("perf_mon_counter_vector", &self.perf_mon_counter_vector);
        builder.field("local_interrupt_0_vector", &self.local_interrupt_0_vector);
        builder.field("local_interrupt_1_vector", &self.local_interrupt_1_vector);
        builder.field("error_interrupt_vector", &self.error_interrupt_vector);
        builder.field("initial_timer_count", &self.initial_timer_count);
        builder.field("current_timer_count", &self.current_timer_count);
        builder.field(
            "divide_configuration_register",
            &self.divide_configuration_register,
        );

        builder.finish()
    }
}

impl Timer for xApic {
    fn get_division_mode(&self) -> u32 {
        let div_value: TimerDivisionMode = self.divide_configuration_register.data.clone().into();
        div_value.to_divide_value().into()
    }

    /// Supported division modes are `1,2,4,8,16,32,64,128`
    fn set_division_mode(&mut self, div: u32) -> TimerResult {
        if div > 128 {
            return Err(TimerError::DivisionModeUnsupported);
        }
        if let Some(mode) = TimerDivisionMode::from_divide_value(div as u8) {
            self.divide_configuration_register.data = mode;
            return Ok(());
        } else {
            Err(TimerError::DivisionModeUnsupported)
        }
    }

    /// Setting the clock on the local APIC starts the clock and setting it to 0 will stop it.
    fn set_clock_count(&mut self, count: u64, mode: crate::time::TimerMode) -> TimerResult {
        if count > u32::MAX as u64 {
            return Err(TimerError::CountTooHigh);
        }
        let mode: TimerMode = mode.try_into()?;
        self.timer_vector.data.set_timer_mode(mode);
        self.initial_timer_count.data = count as u32; // not lossy because of comparison above
        return Ok(());
    }

    fn get_initial_clock(&self) -> Result<u64, TimerError> {
        Ok(self.initial_timer_count.data as u64)
    }
}

struct ThermalVectorCheck;
impl DeviceCheck for ThermalVectorCheck {
    fn exists() -> bool {
        raw_cpuid::CpuId::new()
            .get_feature_info()
            .unwrap()
            .has_acpi()
    }
}

struct PerfMonCheck;
impl DeviceCheck for PerfMonCheck {
    fn exists() -> bool {
        return if let Some(_) = raw_cpuid::CpuId::new().get_performance_monitoring_info() {
            true
        } else {
            false
        };
    }
}

struct NonExistentIGuess;
impl DeviceCheck for NonExistentIGuess {
    fn exists() -> bool {
        false
    }
}

#[repr(C, align(16))]
/// This struct is provided for padding within APIC structs
/// _space is MaybeUninit to discourage compiler meddling
struct ApicReservedRegister {
    _space: MaybeUninit<u128>,
}

// these are defined here because x2apic uses a slightly different definition than the xapic
#[modular_bitfield::bitfield]
#[derive(Debug)]
#[repr(u32)]
#[doc(hidden)]
/// Don't use this outside of this module. This struct if pub because otherwise it emits a warning
pub struct InterruptCommandRegisterHigh {
    #[skip]
    _reserved: modular_bitfield::specifiers::B24,
    #[skip(getters)]
    destination: u8,
}

#[modular_bitfield::bitfield]
#[derive(Debug)]
#[repr(u32)]
#[doc(hidden)]
/// Don't use this outside of this module. This struct if pub because otherwise it emits a warning
pub struct InterruptCommandRegisterLow {
    #[skip(getters)]
    vector: u8,
    #[skip(getters)]
    delivery_mode: InterruptType,
    #[skip(getters)]
    logical_address: bool,
    #[skip(setters)]
    delivery_status: bool,
    #[skip]
    _res0: modular_bitfield::specifiers::B1,
    #[skip(getters)]
    polarity: bool,
    #[skip(getters)]
    level_trigger: bool,
    #[skip]
    _res1: modular_bitfield::specifiers::B2,
    #[skip(getters)]
    dest_shorthand: super::DestinationShorthand,
    #[skip]
    _res2: modular_bitfield::specifiers::B12,
}
