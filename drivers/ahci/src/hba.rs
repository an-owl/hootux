//! This crate provides support for AHCI devices.
//! Documentation for registers within this crate conforms to a general outline.
//!
//! First a mnemonic is provided that can help with identifying the field/register within the
//! specification documentation.
//!
//! An identifier as to how the field/register should be read/written.
//! - (RO) -- Read only; This field may not be modified by software.
//! - (RW) -- Read Write: Software may modify the field. Limitations may be imposed at to when
//! software may write to the bit.
//! - (R1C) -- Read 1 Clear: Hardware may set the bit to `1` Software is not allowed to set the bit
//! to `1`. Software may clear the bit to `0` wy writing `1`.
//! - (RW1) -- Read Write 1: Software may set the bit to `1` but cannot set the bit to `0`
//! - (CD) -- Check documentation for information about read/writing to this register
//!
//! This will be followed by normal documentation of the register/field.

use crate::hba::port_control::PortControl;
use ata::command::constructor;

pub(crate) type SyncCap = alloc::sync::Arc<Capabilities>;

pub(crate) mod command;
pub(crate) mod general_control;
pub(crate) mod port_control;

pub struct HostBusAdapter {
    pub general: &'static mut general_control::GeneralControl,
    /// Vendor specific registers are defined at the hardware level and must be identified by the
    /// vendor/device in the PCI configuration region
    pub vendor: &'static mut [u8; 0x60],
    pub ports: &'static mut [port_control::PortControl; 32],
}

impl HostBusAdapter {
    /// Constructs Self from a pointer to the HBA configuration region.
    pub unsafe fn from_raw(ptr: *mut u8) -> Self {
        let gen: &mut general_control::GeneralControl = &mut *ptr.cast();

        let vendor = &mut *ptr.offset(0xa0).cast();

        // ports
        let ports = &mut *ptr.offset(0x100).cast();

        Self {
            general: gen,
            vendor,
            ports,
        }
    }

    pub fn num_ports(&self) -> u8 {
        self.general.get_capabilities().0.port_count()
    }

    /// Returns a mutable reference to the given port
    pub fn get_port(&mut self, port: u8) -> &mut PortControl {
        &mut self.ports[port as usize]
    }
}

#[repr(C)]
pub enum SataGeneration {
    Gen1 = 1,
    Gen2,
    Gen3,
}

struct CmdIndex {
    index: u8,
}

impl CmdIndex {
    pub fn new(index: u8) -> Option<Self> {
        if index < 32 {
            Some(Self { index })
        } else {
            None
        }
    }
    pub fn index(&self) -> u8 {
        self.index
    }
}

/// Contains HBA capabilities which are relevant to general operation which are not easily
/// accessible downstream of the HBA struct.
// this should be kept in an Arc
pub(crate) struct Capabilities {
    qword_addr: bool,
    queue_depth: u8,
}

impl Capabilities {
    /// returns the memory region used for physical memory allocations.
    pub(crate) fn get_region(&self) -> hootux::mem::buddy_frame_alloc::MemRegion {
        if self.qword_addr {
            hootux::mem::buddy_frame_alloc::MemRegion::Mem64
        } else {
            hootux::mem::buddy_frame_alloc::MemRegion::Mem32
        }
    }

    pub(crate) fn queue_depth(&self) -> u8 {
        self.queue_depth
    }
}

impl From<&general_control::GeneralControl> for Capabilities {
    fn from(value: &general_control::GeneralControl) -> Self {
        Self {
            qword_addr: value.get_capabilities().0.supports_qword_addr(),
            queue_depth: value.get_capabilities().0.get_command_slots(),
        }
    }
}
