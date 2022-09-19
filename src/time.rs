//! This module is for all forms of timekeeping and high level management of timers.
//!
//! each timer module contains a method of finding out the speed of the timer so period to clock
//! calculations can be made at the lowest level possible
//!
//! Timers may not all act exactly the same and their module level documentation should reflect
//! these quirks

pub mod high_precision_event_timer;
pub(crate) type TimerResult = Result<(),TimerError>;

/// This contains a reference to the system timer. Changing the concrete type is VERY UNSAFE and
/// must be synchronised between threads. Failure to do so may cause missed timer interrupts or
/// result in timer interrupts too early.
///
/// The method used to change the concrete type must broadcast to all cpus to switch clock-source.
/// cpus must then switch clock-source and report it's completion when all cpus have switched
/// clock-source the system may return to normal operation
// todo: ensure that this cannot be changed without notifying all cpus
#[thread_local]
static mut IN_USE_TIMER: Option<&dyn Timer> = None;

/// This enums variants reflect the error status of a function.
#[derive(Debug)]
pub enum TimerError {
    /// This is reserved for timers where the clock speed is determined at
    /// runtime and had not yet been determined
    ClockPeriodUnknown,
    /// This variant is returned when setting a division mode which the
    /// hardware is not capable of (i.e. setting the APIC timer to divide by 256)
    DivisionModeUnsupported,
    /// This variant is returned when a requested operation is unsupported by the hardware
    FeatureUnavailable,
    /// Returned when a clock is given a value that is too large for it to handle.
    /// timers may automatically set their division if counts are too high.
    CountTooHigh,
}

pub trait Timer {

    /// This function gets the number of clocks for a given period
    fn get_period(&self) -> Option<usize>;
    fn get_division_mode(&self) -> u32;
    fn set_division_mode(&mut self, div: u32) -> TimerResult;

    /// Sets the timer in clocks and timer mode
    fn set_clock_count(&mut self, count: usize, mode: TimerMode) -> TimerResult;

    /// Sets the timer based on a given timer period via [set_clock_count]
    fn set_clock(&mut self, period: usize, mode: TimerMode) -> TimerResult {
        if let Some(clocks) = self.get_period(){
            self.set_clock_count(clocks * period,mode)
        } else {
            Err(TimerError::ClockPeriodUnknown)
        }


    }
}

/// All supported Timer modes are listed within this enum. Some modes may be unsupported on some
/// timers and their module should document which modes are available. `Periodic` and `OneShot` is
/// supported on mot timers and is a safe choice
#[non_exhaustive]
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum TimerMode {
    OneShot,
    Periodic,
    TscDeadline
}

impl TryInto<crate::interrupts::apic::apic_structures::apic_types::TimerMode> for TimerMode {
    type Error = TimerError;

    /// Returns FeatureUnavailable when mode is not supported by APIC
    #[allow(unreachable_code)] // Self is non_exhaustive
    fn try_into(self) -> Result<crate::interrupts::apic::apic_structures::apic_types::TimerMode, Self::Error> {

        return match self {
            TimerMode::OneShot => Ok(crate::interrupts::apic::apic_structures::apic_types::TimerMode::OneShot),
            TimerMode::Periodic => Ok(crate::interrupts::apic::apic_structures::apic_types::TimerMode::Periodic),
            TimerMode::TscDeadline => Ok(crate::interrupts::apic::apic_structures::apic_types::TimerMode::TscDeadline),
            _ => Err(TimerError::FeatureUnavailable)
        }
    }
}

impl From<crate::interrupts::apic::apic_structures::apic_types::TimerMode> for TimerMode {
    fn from(mode: crate::interrupts::apic::apic_structures::apic_types::TimerMode) -> Self {
        use crate::interrupts::apic::apic_structures::apic_types::TimerMode as OtherMode;
        match mode {
            OtherMode::OneShot => TimerMode::OneShot,
            OtherMode::Periodic => TimerMode::Periodic,
            OtherMode::TscDeadline => TimerMode::TscDeadline,
        }
    }
}

/// Struct to wrap timers to ensure thread safety for global system timers
#[derive(Debug)]
struct ThreadSafeTimer<T: Timer> {
    timer: alloc::sync::Arc<spin::RwLock<T>>
}

impl<T: Timer> ThreadSafeTimer<T> {
    fn new(timer: T) -> Self {
        Self{timer: alloc::sync::Arc::new(
            spin::RwLock::new(
                timer
            ))
        }
    }
}

impl<T: Timer> core::ops::Deref for ThreadSafeTimer<T> {
    type Target = alloc::sync::Arc<spin::RwLock<T>>;

    fn deref(&self) -> &Self::Target {
        &self.timer
    }
}

impl<T: Timer> Clone for ThreadSafeTimer<T> {
    fn clone(&self) -> Self {
        Self{ timer: self.timer.clone() }
    }
}

impl<T: Timer> Timer for ThreadSafeTimer<T> {
    fn get_period(&self) -> Option<usize> {
        self.timer.read().get_period()
    }

    fn get_division_mode(&self) -> u32 {
        self.timer.read().get_division_mode()
    }

    fn set_division_mode(&mut self, div: u32) -> TimerResult {
        self.timer.write().set_division_mode(div)
    }

    fn set_clock_count(&mut self, count: usize, mode: TimerMode) -> TimerResult {
        self.timer.write().set_clock_count(count,mode)
    }

    fn set_clock(&mut self, period: usize, mode: TimerMode) -> TimerResult {
        self.timer.write().set_clock(period,mode)
    }
}