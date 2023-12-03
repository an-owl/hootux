use modular_bitfield::{bitfield, specifiers::*, BitfieldSpecifier};

struct IoApic {
    index: *mut u32,
    data: *mut u32,

    gsi_base: u8,
    size: u8,
}

impl IoApic {
    const ID: u32 = 0;
    const VER: u32 = 1;
    const ARB: u32 = 2;

    unsafe fn write(&mut self, addr: u32, data: u32) {
        // SAFETY: This is safe because self.index is valid and aligned to 16
        self.index.write_volatile(addr);
        // SAFETY: This is not safe because it can be used to modify and enable interrupts
        self.data.write_volatile(data);
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
    fn get_entry(&mut self, index: u8) -> RedirectionTableEntry {
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
    unsafe fn set_entry(&mut self, index: u8, entry: RedirectionTableEntry) {
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
        unsafe { self.write(((index * 2) + 0x10) as u32, masked.seg[0]) };

        // Write the actual entry. High half is first because low half may unmask the interrupt
        self.write(((index * 2) + 0x11) as u32, entry.seg[1]);
        self.write(((index * 2) + 0x10) as u32, entry.seg[0]);
    }
}

struct IoApicVersion {
    data: [u8; 4],
}

impl IoApicVersion {
    /// Returns the IOAPIC revision
    fn revision(&self) -> u8 {
        self.data[0]
    }

    /// Returns the number of redirection entries this device contains
    fn entries(&self) -> u8 {
        self.data[2]
    }
}

const _ASSERT: () =
    assert!(core::mem::size_of::<RedirectionTableEntry>() == core::mem::size_of::<u64>());

#[bitfield]
#[derive(Debug, Copy, Clone)]
struct RedirectionTableEntry {
    ///
    /// The target vector for the interrupt.
    vector: u8,
    delivery_mode: DeliveryMode,
    destination_mode: DestinationMode,
    #[skip(getters)]
    ///
    /// Delivery status bit. This is set to `true` when an interrupt is pending
    pending: bool,
    polarity: PinPolarity,
    #[skip(setters)]
    ///
    /// When this interrupt uses level triggering, and the interrupt has been accepted by a LAPIC
    /// this will be set to 1 util EOI has been sent for this interrupt.
    /// When the interrupt is edge triggered the state of this bit is undefined
    remote_irr: bool,
    trigger_mode: TriggerMode,
    mask: bool,
    #[skip]
    __: B39,
    ///
    /// Destination field.
    ///
    /// When the destination mode is logical this is a bitfield
    destination: u8,
}

#[derive(BitfieldSpecifier, Debug)]
#[bits = 3]
enum DeliveryMode {
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

#[derive(BitfieldSpecifier, Debug)]
#[bits = 1]
#[repr(C)]
/// Destination mode bit.
///
/// - Physical mode selects the destination using its APIC id in bits \[56..=59\] (4bits).
/// - Logical mode selects the destination using a bitfield in bits \[56..=63\] (8bits).
enum DestinationMode {
    PhysicalMode = 0,
    LogicalMode,
}

#[derive(BitfieldSpecifier, Debug)]
#[bits = 1]
#[repr(C)]
/// Indicated the type of signal used to assert the interrupt.
enum TriggerMode {
    EdgeTriggered = 0,
    LevelTriggered,
}

#[derive(BitfieldSpecifier, Debug)]
#[bits = 1]
#[repr(C)]
/// Interrupt pin polarity bit.
enum PinPolarity {
    AssertHigh = 0,
    AssertLow,
}
