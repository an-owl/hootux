//! This module is intended for private kernel structures, however there are some public structs in
//! here that are intended to be available from other places.
//!
//! The structs here are for hardware configurations which is abstracted by the kernel via other modules.

use crate::interrupts::apic::ioapic::{PinPolarity, TriggerMode};
use acpi::InterruptModel;
use acpi::platform::interrupt::Polarity;

pub struct SystemctlResources {
    pub(crate) ioapic: GlobalIoApic,
}

impl SystemctlResources {
    pub(super) const fn new() -> Self {
        Self {
            ioapic: GlobalIoApic {
                inner: spin::Mutex::new(alloc::vec::Vec::new()),
                overrides: Once(core::cell::OnceCell::new()),
            },
        }
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
// pls throw error if this is not x86, I'll deal with the rest later
pub(crate) struct GlobalIoApic {
    inner: spin::Mutex<
        alloc::vec::Vec<(
            crate::interrupts::apic::ioapic::IoApic,
            crate::util::UnsafeBox<[u8; 16], crate::mem::write_combining::WcMmioAlloc>,
        )>,
    >,
    overrides: Once<alloc::boxed::Box<[InterruptOverride]>>,
}

impl GlobalIoApic {
    /// Attempts to set the requested the requested GSI to the given configuration.
    /// If a controller for the requested cannot be found this fn will return `Err(())`
    pub(crate) unsafe fn set_gsi(
        &self,
        gsi: u8,
        cfg: crate::interrupts::apic::ioapic::RedirectionTableEntry,
    ) -> Result<(), ()> {
        unsafe {
            let mut l = self.inner.lock();
            for (i, _) in &mut *l {
                if i.is_gsi(gsi) {
                    i.set_gsi(gsi, cfg);
                    return Ok(());
                }
            }
            Err(())
        }
    }

    /// Attempts to fetch the configuration for the requested GSI.
    /// If a controller for the requested cannot be found this fn will return `Err(())`
    pub(crate) fn get_gsi(
        &self,
        gsi: u8,
    ) -> Result<crate::interrupts::apic::ioapic::RedirectionTableEntry, ()> {
        let mut l = self.inner.lock();
        for (i, _) in &mut *l {
            if i.is_gsi(gsi) {
                return Ok(i.get_gsi(gsi));
            }
        }
        Err(())
    }

    pub(super) fn cfg_madt(&self, madt: core::pin::Pin<&acpi::madt::Madt>) {
        let parsed = madt
            .parse_interrupt_model_in(alloc::alloc::Global)
            .expect("Failed to parse interrupt model")
            .0;

        let mut arr = alloc::vec::Vec::new();

        match parsed {
            InterruptModel::Unknown => unimplemented!(),
            InterruptModel::Apic(a) => {
                for i in a.io_apics.iter() {
                    let addr = i.address;
                    let gsi_base = i.global_system_interrupt_base;
                    let b = crate::util::UnsafeBox::new(unsafe {
                        crate::mem::write_combining::WcMmioAlloc::new(addr as u64)
                    });

                    // SAFETY: References "will be dropped first", these should actually ever be
                    // dropped for the lifetime of the system.
                    let ptr: *mut [u8; 16] = unsafe { b.ptr() };

                    // SAFETY: The address is given by firmware, if its is wrong then there is a firmware bug.
                    let apic = unsafe {
                        crate::interrupts::apic::ioapic::IoApic::new(ptr.cast(), gsi_base)
                    };

                    // this is safe, the addr is given by APCI firmware
                    arr.push((apic, b));
                }

                let overrides: alloc::vec::Vec<InterruptOverride> = a
                    .interrupt_source_overrides
                    .iter()
                    .map(|f| f.into())
                    .collect();
                self.overrides
                    .0
                    .set(overrides.into_boxed_slice())
                    .expect("ISA overrides already set");
            }
            _ => unimplemented!(),
        }

        *self.inner.lock() = arr;
    }

    pub(crate) fn lookup_override(&self, isa: u8) -> Option<InterruptOverride> {
        for i in self.overrides.0.get().unwrap().iter() {
            if i.isa_source == isa {
                return Some(*i);
            }
        }
        None
    }
}

/// Sync + Send newtype for [core::cell::OnceCell]
struct Once<T>(core::cell::OnceCell<T>);

// SAFETY: GlobalApic uses OnceCell which is racy, however GlobalApic is initialized before MP
// initialization so race conditions cannot occur.
unsafe impl<T> Sync for Once<T> {}
unsafe impl<T> Send for Once<T> {}

#[derive(Debug, Copy, Clone)]
/// Legacy "ISA" interrupts attached to PIC8259's are usually identity mapped to GSIs.
/// Where an ISA interrupt is overridden it is mapped to the GSI provided.
/// An ISA interrupt may be identity mapped however override the trigger mode/polarity
/// In the case the trigger mode/polarity is `None` the value is not overridden and the consumer
/// must determine their values.
pub struct InterruptOverride {
    pub isa_source: u8,
    pub global_system_interrupt: u32,
    pub polarity: Option<PinPolarity>,
    pub trigger_mode: Option<TriggerMode>,
}

impl From<&acpi::platform::interrupt::InterruptSourceOverride> for InterruptOverride {
    fn from(value: &acpi::platform::interrupt::InterruptSourceOverride) -> Self {
        let trigger_mode = match value.trigger_mode {
            acpi::platform::interrupt::TriggerMode::SameAsBus => None,
            acpi::platform::interrupt::TriggerMode::Edge => Some(TriggerMode::EdgeTriggered),
            acpi::platform::interrupt::TriggerMode::Level => Some(TriggerMode::LevelTriggered),
        };

        let polarity = match value.polarity {
            Polarity::SameAsBus => None,
            Polarity::ActiveHigh => Some(PinPolarity::AssertHigh),
            Polarity::ActiveLow => Some(PinPolarity::AssertLow),
        };

        Self {
            isa_source: value.isa_source,
            global_system_interrupt: value.global_system_interrupt,
            polarity,
            trigger_mode,
        }
    }
}
