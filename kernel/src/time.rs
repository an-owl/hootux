//! This module is for all forms of timekeeping and high level management of timers.
//!
//! each timer module contains a method of finding out the speed of the timer so period to clock
//! calculations can be made at the lowest level possible
//!
//! Timers may not all act exactly the same and their module level documentation should reflect
//! these quirks

use core::pin::Pin;
use core::task::{Context, Poll};

pub mod acpi_pm_timer;
pub(crate) type TimerResult = Result<(), TimerError>;

static SYSTEM_TIME: SystemTime = SystemTime::new();
static SYSTEM_TIMEKEEPER: spin::RwLock<Option<alloc::boxed::Box<(dyn TimeKeeper + Sync + Send)>>> =
    spin::RwLock::new(None);

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
    fn get_division_mode(&self) -> u32;
    fn set_division_mode(&mut self, div: u32) -> TimerResult;

    /// Sets the timer in clocks and timer mode
    fn set_clock_count(&mut self, count: u64, mode: TimerMode) -> TimerResult;

    fn get_initial_clock(&self) -> Result<u64, TimerError>;
}

/// All supported Timer modes are listed within this enum. Some modes may be unsupported on some
/// timers and their module should document which modes are available. `Periodic` and `OneShot` is
/// supported on mot timers and is a safe choice
#[non_exhaustive]
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum TimerMode {
    OneShot,
    Periodic,
    TscDeadline,
}

impl TryInto<crate::interrupts::apic::apic_structures::apic_types::TimerMode> for TimerMode {
    type Error = TimerError;

    /// Returns FeatureUnavailable when mode is not supported by APIC
    #[allow(unreachable_patterns)] // Self is non_exhaustive
    fn try_into(
        self,
    ) -> Result<crate::interrupts::apic::apic_structures::apic_types::TimerMode, Self::Error> {
        return match self {
            TimerMode::OneShot => {
                Ok(crate::interrupts::apic::apic_structures::apic_types::TimerMode::OneShot)
            }
            TimerMode::Periodic => {
                Ok(crate::interrupts::apic::apic_structures::apic_types::TimerMode::Periodic)
            }
            TimerMode::TscDeadline => {
                Ok(crate::interrupts::apic::apic_structures::apic_types::TimerMode::TscDeadline)
            }
            _ => Err(TimerError::FeatureUnavailable),
        };
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

/// Struct for recording duration since boot
struct SystemTime {
    dirty: core::sync::atomic::AtomicBool,
    /// time recorded when self was last updated
    time: core::sync::atomic::AtomicU64,

    /// timer count when self was last updated
    last_check: core::sync::atomic::AtomicU64,
}

impl SystemTime {
    const fn new() -> Self {
        use core::sync::atomic;
        Self {
            dirty: atomic::AtomicBool::new(false),
            time: atomic::AtomicU64::new(0),
            last_check: atomic::AtomicU64::new(0),
        }
    }

    /// Returns the current time in nanoseconds since boot
    fn get_system_time(&self) -> u64 {
        if !self.update() {
            // This will prevent deadlocks when trying to get the time. But attempt to get the up
            // to date value. This will introduce jitter to the system clock but im not too
            // concerned with that
            for _ in 0..50 {
                if !self.dirty.load(atomic::Ordering::Acquire) {
                    break;
                }
                core::hint::spin_loop()
            }
        }
        self.time.load(atomic::Ordering::Relaxed)
    }

    /// Sets self to time 0 and records the clock count
    fn init(&self) {
        self.last_check.store(
            SYSTEM_TIMEKEEPER.read().as_ref().unwrap().time_since(0).1,
            atomic::Ordering::Relaxed,
        )

        // panics if called before `kernel_init_timer`
    }

    /// Syncs the system time with the current clock count
    fn update(&self) -> bool {
        if let Ok(_) = self.dirty.compare_exchange_weak(
            false,
            true,
            atomic::Ordering::Acquire,
            atomic::Ordering::Relaxed,
        ) {
            let (period, recorded_clock) = SYSTEM_TIMEKEEPER
                .read()
                .as_ref()
                .unwrap()
                .time_since(self.last_check.load(atomic::Ordering::Relaxed));
            self.time.fetch_add(period, atomic::Ordering::Relaxed);
            self.last_check
                .store(recorded_clock, atomic::Ordering::Relaxed);
            self.dirty.store(false, atomic::Ordering::Release);
            true
        } else {
            false
        }
    }
}

/// Trait for calculating durations should be used for system timekeeping
pub trait TimeKeeper {
    /// Returns `(duration, clock_read)` since last read. where clock read is the value read from
    /// the clock used to calculate the duration
    ///
    /// Implementations must account for handle rollover
    fn time_since(&self, old_time: u64) -> (u64, u64);
}

/// Returns the current time in nanoseconds since boot.
pub fn get_sys_time() -> u64 {
    // todo: speed up with try read. on fail skip update
    SYSTEM_TIME.get_system_time()
}

/// Attempts to update the system timer. If the timer is already being updated this will  
pub(crate) fn update_timer() {
    SYSTEM_TIME.update();
}

pub fn kernel_init_timer(timer: alloc::boxed::Box<(impl TimeKeeper + Sync + Send + 'static)>) {
    // This takes ownership but does not compile without 'static. WHY?
    *SYSTEM_TIMEKEEPER.write() = Some(timer);
    SYSTEM_TIME.init();
}

#[derive(Copy, Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
pub struct Duration {
    nanos: u64,
}

impl Duration {
    pub fn seconds(secs: u64) -> Self {
        Self {
            nanos: secs * 1000000000,
        }
    }

    pub fn millis(milis: u64) -> Self {
        Self {
            nanos: milis * 1000000,
        }
    }

    pub fn nanos(nanos: u64) -> Self {
        Self { nanos }
    }

    pub fn get_nanos(&self) -> u64 {
        self.nanos
    }
}

pub struct AbsoluteTime {
    nanos: u64
}

impl AbsoluteTime {

    /// Returns true if the time represented by self has passed.
    pub fn is_future(&self) -> bool {
         self.nanos > get_sys_time()
    }
}

impl From<Duration> for AbsoluteTime {
    fn from(value: Duration) -> Self {
        Self {
            nanos: get_sys_time() + value.nanos
        }
    }
}

impl core::fmt::Display for Duration {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let sec = self.nanos / 1000000000;
        let rem = self.nanos % 1000000000;

        write!(f, "{}.{}", sec, rem)
    }
}
