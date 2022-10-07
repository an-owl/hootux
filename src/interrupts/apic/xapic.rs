use crate::device_check::{DeviceCheck, MaybeExists};
use crate::interrupts::apic::apic_structures::registers::{ApicErrorInt, InternalInt, LocalInt, TimerIntVector};
use super::apic_structures::{apic_types::*, registers::*};
use super::{Apic};
use core::fmt::{Debug, Formatter};
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use crate::time::{Timer, TimerError, TimerResult};


#[allow(non_camel_case_types)]
#[repr(C,align(4096))]
/// Struct representing the cpu's Local xAPIC
/// This does not come with a constructor because it should already be resident within memory
/*
 * i cannot figure out if arbitration_priority or remote_read is present. The manual says its not
 * present on pentium 4 class cpus. but then directs you to finding Nehalem cpus which is it intel?
 * either way they're present but inaccessible until i figure it out
 */

pub struct xApic{
    _reserved0: [ApicReservedRegister;2],
    id_register: AlignedRegister<u32>,
    version_register: AlignedRegister<u32>,
    _reserved1: [ApicReservedRegister;4],

    task_priority: AlignedRegister<u32>,
    arbitration_priority:  MaybeExists<AlignedRegister<WrappedDword>,NonExistentIGuess>, // i just cannot figure where these are available
    processor_priority: AlignedRegister<u32>,
    eoi_reg: EoiRegister,

    remote_read: MaybeExists<AlignedRegister<u32>,NonExistentIGuess>, // see arbitration_priority

    logical_destination: AlignedRegister<u32>,
    destination_format: AlignedRegister<u32>,

    spurious_interrupt_vector:  AlignedRegister<SpuriousVector>,

    in_service: [AlignedRegister<u32>;8],
    trigger_mode: [AlignedRegister<u32>; 8],
    interrupt_request: [AlignedRegister<u32>; 8],

    error_status: AlignedRegister<ApicError>,

    _reserved2: [ApicReservedRegister; 6],

    cmc_vector: AlignedRegister<InternalInt>,

    interrupt_command_register: [AlignedRegister<WrappedDword>;2],

    timer_vector: AlignedRegister<TimerIntVector>,
    thermal_sensor_vector: MaybeExists<AlignedRegister<InternalInt>, ThermalVectorCheck>, // not available on all processors
    perf_mon_counter_vector: MaybeExists<AlignedRegister<InternalInt>, PerfMonCheck >, // not available on all processors
    local_interrupt_0_vector: AlignedRegister<LocalInt>,
    local_interrupt_1_vector: AlignedRegister<LocalInt>,
    error_interrupt_vector: AlignedRegister<ApicErrorInt>,

    initial_timer_count: AlignedRegister<u32>,
    current_timer_count: AlignedRegister<u32>,
    _reserved: [ApicReservedRegister;4],
    divide_configuration_register: AlignedRegister<TimerDivisionMode>,

    _reserved4: ApicReservedRegister, // offset should be 0x3f0
}

impl Apic for xApic {
    unsafe fn set_enable(&mut self, enable: bool) {
        self.spurious_interrupt_vector.data.set(SpuriousVector::APIC_ENABLE,enable);
    }

    unsafe fn init_err(&mut self, vector: u8, mask: bool) {

        self.error_status.data = ApicError::empty();
        unsafe {
            self.error_interrupt_vector.data.set_vector(vector,InterruptDeliveryMode::Fixed);
            self.error_interrupt_vector.data.set_mask(mask);
        }
    }

    unsafe fn init_timer(&mut self, vector: u8, mask: bool) {
        unsafe {
            self.timer_vector.data.set_vector(vector,InterruptDeliveryMode::Fixed);
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
}

impl xApic{

    pub fn fetch_addr() -> ApicBaseData {
        unsafe {
            use x86_msr::Msr;
            x86_msr::architecture::ApicBase::read()
        }
    }

    pub fn gen_err(&mut self) {
        for i in &mut self._reserved{
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

    pub fn set_spurious_vec(&mut self, vector: u8){
        self.spurious_interrupt_vector.data.set_vector(vector);
    }

}




#[repr(C,align(16))]
/// This struct is for interacting with apic registers. All registers are 32bits aligned to 128bits
/// This ensures the correct read/write operations are used while maintaining alignment
///
/// This is also used to enable simple forward compatibility with the x2apic
struct AlignedRegister<T>{
    data: T
}



impl<T: Debug> Debug for AlignedRegister<T>{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}" , self.data)
    }
}

#[derive(PartialEq,PartialOrd,Clone)]
/// Some APIC registers can be interpreted as a normal u32 this struct provides this
struct WrappedDword(u32);

impl Deref for WrappedDword{
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for WrappedDword{
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
        builder.field("id",&self.id_register.data.clone());
        builder.field("version",&self.version_register.data.clone());
        builder.field("task_priority",&self.task_priority.data.clone());
        builder.field("arbitration_priority", &self.arbitration_priority);
        builder.field("processor_priority",&self.processor_priority.data.clone());
        builder.field("remote_read", &self.remote_read);
        builder.field("logical_destination", &self.logical_destination.data.clone());
        builder.field("destination_format", &self.destination_format.data.clone());
        builder.field("spurious_interrupt_vector", &self.spurious_interrupt_vector.data.clone());
        builder.field("in_service", &self.in_service);
        builder.field("trigger_mode", &self.trigger_mode);
        builder.field("interrupt_request", &self.interrupt_request);
        builder.field("error_status", &self.error_status.data.clone());
        builder.field("cmc_vector", &self.cmc_vector.data.get_reg());
        builder.field("interrupt_command_register", &self.interrupt_command_register);
        builder.field("timer_vector",&self.timer_vector);
        builder.field("thermal_sensor_vector", &self.thermal_sensor_vector);
        builder.field("perf_mon_counter_vector", &self.perf_mon_counter_vector);
        builder.field("local_interrupt_0_vector", &self.local_interrupt_0_vector);
        builder.field("local_interrupt_1_vector", &self.local_interrupt_1_vector);
        builder.field("error_interrupt_vector", &self.error_interrupt_vector);
        builder.field("initial_timer_count", &self.initial_timer_count);
        builder.field("current_timer_count", &self.current_timer_count);
        builder.field("divide_configuration_register", &self.divide_configuration_register);

        builder.finish()

    }
}

impl Timer for xApic {
    fn get_period(&self) -> Option<usize> {
        unsafe { CLOCKS_PER_USEC }
    }

    fn get_division_mode(&self) -> u32 {
        let div_value: TimerDivisionMode = self.divide_configuration_register.data.clone().into();
        div_value.to_divide_value().into()

    }

    /// Supported division modes are `1,2,4,8,16,32,64,128`
    fn set_division_mode(&mut self, div: u32) -> TimerResult {
        if div > 128 {
            return Err(TimerError::DivisionModeUnsupported)
        }
        if let Some(mode) = TimerDivisionMode::from_divide_value(div as u8) {
            self.divide_configuration_register.data = mode;
            return Ok(())
        } else { Err(TimerError::DivisionModeUnsupported) }
    }

    /// Setting the clock on the local APIC starts the clock and setting it to 0 will stop it.
    fn set_clock_count(&mut self, count: usize, mode: crate::time::TimerMode) -> TimerResult {
        if count > u32::MAX as usize {
            return Err(TimerError::CountTooHigh)
        }
        let mode: TimerMode = mode.try_into()?;
        self.timer_vector.data.set_timer_mode(mode);
        self.initial_timer_count.data = count as u32; // not lossy because of comparison above
        return Ok(())
    }
}

struct ThermalVectorCheck;
impl DeviceCheck for ThermalVectorCheck{
    fn exists() -> bool {
        raw_cpuid::CpuId::new().get_feature_info().unwrap().has_acpi()
    }
}

struct PerfMonCheck;
impl DeviceCheck for PerfMonCheck{
    fn exists() -> bool {
        return if let Some(_) = raw_cpuid::CpuId::new().get_performance_monitoring_info() {
            true
        } else { false }
    }
}

struct NonExistentIGuess;
impl DeviceCheck for NonExistentIGuess{
    fn exists() -> bool {
        false
    }
}

#[repr(C,align(16))]
/// This struct is provided for padding within APIC structs
/// _space is MaybeUninit to discourage compiler meddling
struct ApicReservedRegister{
    _space: MaybeUninit<u128>
}