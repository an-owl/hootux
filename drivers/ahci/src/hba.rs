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

pub(crate) mod command;
pub(crate) mod general_control;
pub(crate) mod port_control;

#[derive(Debug)]
pub(crate) struct HostBusAdapter {
    pub general: &'static mut general_control::GeneralControl,
    /// Vendor specific registers are defined at the hardware level and must be identified by the
    /// vendor/device in the PCI configuration region
    pub vendor: &'static mut [u8; 0x60],
    // An unspecified number of ports are implemented, this contains all ports which are mapped by ABAR
    pub ports: alloc::vec::Vec<&'static mut port_control::PortControl>,
}

impl HostBusAdapter {
    /// Constructs Self from a pointer to the HBA configuration region.
    pub unsafe fn from_raw(ptr: &mut [u8]) -> Self {
        unsafe {
            let r#gen: &mut general_control::GeneralControl = &mut *ptr.as_mut_ptr().cast();

            let vendor = &mut *ptr.as_mut_ptr().offset(0xa0).cast();

            let port_count = ((ptr.len() - 0x100) / 0x80).min(32); // Each port gets 80h bytes, does not use them all
            let mut arr = alloc::vec::Vec::with_capacity(port_count);

            for i in 0..port_count {
                arr.push(
                    &mut *ptr
                        .as_mut_ptr()
                        .offset(
                            (i * core::mem::size_of::<port_control::PortControl>()) as isize
                                + 0x100,
                        )
                        .cast(),
                )
            }

            Self {
                general: r#gen,
                vendor,
                ports: arr,
            }
        }
    }
}

#[repr(C)]
pub enum SataGeneration {
    Gen1 = 1,
    Gen2,
    Gen3,
}
