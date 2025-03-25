use modular_bitfield::{BitfieldSpecifier, bitfield, specifiers::*};

pub(crate) struct IoApic {
    index: *mut u32,
    data: *mut u32,

    gsi_base: u8,
    size: u8,
}

impl IoApic {
    #[allow(unused)]
    // Index of the Identification register
    const ID: u32 = 0;

    #[allow(unused)]
    // Index of the version register
    const VER: u32 = 1;

    #[allow(unused)]
    // Index of the arbitration register
    const ARB: u32 = 2;

    /// Initializes `Self` from `ptr` this will use the the region from `ptr..ptr+0x10`
    ///
    /// # Panics
    ///
    /// This fn will panic if `gsi_base` is above `255`
    ///
    /// # Safety
    ///
    /// The caller must ensure that
    ///
    /// - `ptr` actually points to a memory mapped region containing a 82093 IO-APIC compatible device.
    /// - The region is mapped
    /// - The region is writable
    /// - The region is mapped using a MMIO compatible memory type (UC/WC)  
    pub(crate) unsafe fn new(ptr: *mut u8, gsi_base: u32) -> Self {
        unsafe {
            let mut s = Self {
                index: ptr.cast(),
                data: ptr.offset(0x10).cast(),
                gsi_base: gsi_base.try_into().expect(&alloc::format!(
                    "Firmware bug? Found GSI base {} max should be 239",
                    gsi_base
                )),
                size: 0,
            };

            s.size = s.get_version().entries();
            s
        }
    }

    unsafe fn write(&mut self, addr: u32, data: u32) {
        unsafe {
            // SAFETY: This is safe because self.index is valid and aligned to 16
            self.index.write_volatile(addr);
            // SAFETY: This is not safe because it can be used to modify and enable interrupts
            self.data.write_volatile(data);
        }
    }

    fn read(&mut self, addr: u32) -> u32 {
        // SAFETY: This is safe because self.index is valid and aligned to 16
        unsafe {
            self.index.write_volatile(addr);
            self.data.read_volatile()
        }
    }

    /// Returns the IO-APIC version and number of redirection entries entries
    fn get_version(&mut self) -> IoApicVersion {
        IoApicVersion {
            data: self.read(Self::VER).to_le_bytes(),
        }
    }

    /// Loads and returns the entry for `index`
    ///
    /// This fn takes `self` as mutable because it must modify the index register in order to read
    /// from the correct register
    pub(crate) fn get_entry(&mut self, index: u8) -> RedirectionTableEntry {
        assert!(
            index < self.size,
            "Attempted to read invalid entry {}: Max {} tried to read {index}",
            self.size,
            core::any::type_name::<Self>()
        );
        let mut bytes: [u8; 8] = [0; 8];
        bytes[..4].copy_from_slice(&self.read(((index * 2) + 0x10) as u32).to_le_bytes());
        bytes[4..].copy_from_slice(&self.read(((index * 2) + 0x11) as u32).to_le_bytes());
        RedirectionTableEntry::from_bytes(bytes)
    }

    /// Sets the entry at `index` to `entry`.
    /// This will mask the interrupt before updating the register to prevent erroneous interrupts.
    ///
    /// # Safety
    ///
    /// This fn is unsafe because it modifies interrupt behaviour.
    /// The caller must ensure the interrupt is properly received and handled.
    #[allow(unsafe_op_in_unsafe_fn)]
    // unsafe here is to mark that it is safe
    pub(crate) unsafe fn set_entry(&mut self, index: u8, entry: RedirectionTableEntry) {
        unsafe {
            assert!(
                index < self.size,
                "Attempted to write invalid entry {}: Max {} tried to read {index}",
                self.size,
                core::any::type_name::<Self>()
            );
            // union allows clearer casting
            union Cvt {
                whole: RedirectionTableEntry,
                seg: [u32; 2],
            }

            let entry = Cvt { whole: entry };

            let masked = Cvt {
                whole: {
                    let mut t = RedirectionTableEntry::new();
                    t.set_mask(true);
                    t
                },
            };

            // SAFETY: This masks the interrupt, this is safe.
            self.write(((index * 2) + 0x10) as u32, masked.seg[0]);

            // Write the actual entry. High half is first because low half may unmask the interrupt
            self.write(((index * 2) + 0x11) as u32, entry.seg[1]);
            self.write(((index * 2) + 0x10) as u32, entry.seg[0]);
        }
    }

    pub(crate) fn is_gsi(&self, gsi: u8) -> bool {
        gsi > self.gsi_base && gsi < self.gsi_base + self.size
    }

    /// Returns the [RedirectionTableEntry] for the requested GSI
    ///
    /// # Panics
    ///
    /// Ths fn will panic if `self` does not own the requested GSI. [Self::is_gsi] will return
    /// whether this fn will panic
    pub(crate) fn get_gsi(&mut self, gsi: u8) -> RedirectionTableEntry {
        assert!(self.is_gsi(gsi));
        self.get_entry(gsi - self.gsi_base)
    }

    /// Sets the [RedirectionTableEntry] for the requested GSI.
    ///
    /// # Panics
    ///
    /// Ths fn will panic if `self` does not own the requested GSI. [Self::is_gsi] will return
    /// whether this fn will panic
    pub(crate) unsafe fn set_gsi(&mut self, gsi: u8, entry: RedirectionTableEntry) {
        unsafe {
            assert!(self.is_gsi(gsi));
            self.set_entry(gsi - self.gsi_base, entry);
        }
    }
}

// SAFETY: This is safe to send across threads, the pointers are not accessible outside self
unsafe impl Send for IoApic {}

pub(crate) struct IoApicVersion {
    data: [u8; 4],
}

#[allow(dead_code)]
impl IoApicVersion {
    /// Returns the IOAPIC revision

    pub fn revision(&self) -> u8 {
        self.data[0]
    }

    /// Returns the number of redirection entries this device contains
    pub fn entries(&self) -> u8 {
        self.data[2]
    }
}

const _ASSERT: () =
    assert!(core::mem::size_of::<RedirectionTableEntry>() == core::mem::size_of::<u64>());

#[bitfield]
#[derive(Debug, Copy, Clone)]
pub struct RedirectionTableEntry {
    ///
    /// The target vector for the interrupt.
    pub vector: u8,
    pub delivery_mode: DeliveryMode,
    pub destination_mode: DestinationMode,
    #[skip(setters)]
    ///
    /// Delivery status bit. This is set to `true` when an interrupt is pending
    pub pending: bool,
    pub polarity: PinPolarity,
    #[skip(setters)]
    ///
    /// When this interrupt uses level triggering, and the interrupt has been accepted by a LAPIC
    /// this will be set to 1 util EOI has been sent for this interrupt.
    /// When the interrupt is edge triggered the state of this bit is undefined
    pub remote_irr: bool,
    pub trigger_mode: TriggerMode,
    pub mask: bool,
    #[skip]
    __: B39,
    ///
    /// Destination field.
    ///
    /// When the destination mode is logical this is a bitfield
    pub destination: u8,
}

#[derive(BitfieldSpecifier, Debug, Copy, Clone)]
#[bits = 3]
pub enum DeliveryMode {
    /// Fixed vector, normal interrupt mode.
    Fixed = 0,
    /// In logical mode this will be accepted by the CPU with the lowest task priority.
    LowestPriority = 1,
    /// System Management Interrupt (im not actually sure what this does).
    /// The delivery mode must use edge.
    /// The local vector must be set to `0` all other values are UB.
    Smi,
    /// Delivers a Non-Maskable Interrupt.
    /// This is edge triggered regardless of the trigger mode bit.
    /// Vector information is ignored.
    Nmi = 4,
    /// Asserts an INIT signal to the targeted CPUs.
    /// The delivery mode must be set to edge using level is UB
    // y tho?
    Init,
    /// Deliver the interrupt as an external PIC8259 compatible device.
    ExtInt = 7,
}

#[derive(BitfieldSpecifier, Debug, Copy, Clone)]
#[bits = 1]
#[repr(C)]
/// Destination mode bit.
///
/// - Physical mode selects the destination using its APIC id in bits \[56..=59\] (4bits).
/// - Logical mode selects the destination using a bitfield in bits \[56..=63\] (8bits).
pub enum DestinationMode {
    PhysicalMode = 0,
    LogicalMode,
}

#[derive(BitfieldSpecifier, Debug, Copy, Clone)]
#[bits = 1]
#[repr(C)]
/// Indicated the type of signal used to assert the interrupt.
pub enum TriggerMode {
    EdgeTriggered = 0,
    LevelTriggered,
}

#[derive(BitfieldSpecifier, Debug, Copy, Clone)]
#[bits = 1]
#[repr(C)]
/// Interrupt pin polarity bit.
pub enum PinPolarity {
    AssertHigh = 0,
    AssertLow,
}

/// This struct represents a Global System Interrupt which is bound to an IRQ
/// GlobalSystemInterrupt represents the configuration of its entry in an IO-APIC.
/// When fields are modified [GlobalSystemInterrupt::set] must be called in order to update the
/// interrupt configuration.
///
/// **Note:** The default mask value is true i.e. interrupts will not occur
// todo disallow setting target CPU.
// Instead give vague configuration mode. Single, Group, All, Low priority, ect.
#[derive(Clone, Debug)]
pub struct GlobalSystemInterrupt {
    vector: u8,
    gsi: u8,
    pub target: Target,
    pub polarity: PinPolarity,
    pub trigger_mode: TriggerMode,
    pub delivery_mode: DeliveryMode,
    pub mask: bool,
}

impl GlobalSystemInterrupt {
    pub(in crate::interrupts) const fn new(gsi: u8, vector: u8) -> Self {
        Self {
            vector,
            gsi,
            target: Target::Physical(PhysicalTarget::new(0)),
            polarity: PinPolarity::AssertHigh,
            trigger_mode: TriggerMode::EdgeTriggered,
            delivery_mode: DeliveryMode::Fixed,
            mask: true,
        }
    }

    /// Returns a compiled GSI entry
    fn rt_entry(&self) -> RedirectionTableEntry {
        let mut rte = RedirectionTableEntry::new();
        match self.target {
            Target::Logical(t) => {
                rte.set_destination(t);
                rte.set_destination_mode(DestinationMode::LogicalMode);
            }
            Target::Physical(PhysicalTarget { id: t }) => {
                rte.set_destination(t);
                rte.set_destination_mode(DestinationMode::PhysicalMode);
            }
        };
        rte.set_polarity(self.polarity);
        rte.set_trigger_mode(self.trigger_mode);
        rte.set_delivery_mode(self.delivery_mode);
        rte.set_mask(self.mask);
        rte.set_vector(self.vector);

        rte
    }

    /// Sets the GSI configuration on the appropriate IO-APIC controller,
    /// returning whether it completed successfully
    ///
    /// # Safety
    ///
    /// The caller must ensure the interrupt is handled properly.
    /// This fn does not set configure the interrupt handler this should be configured before
    /// this fn is called.  
    pub unsafe fn set(&self) -> Result<(), ()> {
        unsafe {
            let apic = &crate::system::sysfs::get_sysfs().systemctl.ioapic;
            apic.set_gsi(self.gsi, self.rt_entry())
        }
    }

    /// Returns the [RedirectionTableEntry] for the GSI represented by `self` This can be used for
    /// debugging purposes
    pub fn get(&self) -> Result<RedirectionTableEntry, ()> {
        let apic = &crate::system::sysfs::get_sysfs().systemctl.ioapic;
        apic.get_gsi(self.gsi)
    }
}

#[derive(Debug, Clone)]
pub struct PhysicalTarget {
    id: u8, // only 4 bits usable
}

impl PhysicalTarget {
    pub const fn new(id: u8) -> Self {
        assert!(id < 16);
        Self { id }
    }

    pub fn set(&mut self, id: u8) {
        assert!(id < 16);
        self.id = id;
    }
}

#[derive(Debug, Clone)]
pub enum Target {
    Logical(u8),
    Physical(PhysicalTarget),
}
