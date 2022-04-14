#![no_std]
//! This crate contains features to manipulate Model Specific Registers (msr) on ia-32 and x86_64 architectures
//!
//! For information on x86_64 MSRs see <https://www.intel.com/content/dam/develop/public/us/en/documents/335592-sdm-vol-4.pdf> and <https://www.amd.com/system/files/TechDocs/24593.pdf>
//!
//! The traits in this module are pub for custom implementations

pub mod architecture;
use lazy_static::lazy_static;


lazy_static!{
    /// contains the MAXPHYADDR as reported by `cpuid`
    static ref MAXPHYADDR: u8 = {
        let reg = cpuid_lookup_reg(CpuidRegister::eax, 0x8000_0008, 0);
        let count = reg as u8; // thanks for making it easy
        count
    };
    /// This contains a bitmask for retrieving MAXPHYADDR types
    static ref MAXPHYADDR_MASK: u64 = {
        let mask = !(u64::MAX << *MAXPHYADDR);
        mask
    };
}

/// This trait can be implemented on MSR types to read and write to the msr located at `MSR_addr`
pub trait Msr<R: MsrFlags>{
    const MSR_ADDR: u32;

    /// Reads MSR specified by `MSR_ADDR`
    ///
    /// This function is unsafe because it may cause a cpu fault
    /// because it reads regardless of whether it is legal or not
    unsafe fn read() -> R {

        let low: u32;
        let high: u32;

        core::arch::asm!(
        "rdmsr",
        in("ecx") Self::MSR_ADDR,
        out("eax") low,
        out("edx") high,
        );

        let r = MsrFlags::new(high,low);
        r



    }

    /// Writes `write` to the given register
    ///
    /// This function is unsafe because it may cause a cpu fault
    /// because it writes regardless of whether it is legal or not.
    /// It also changes the internal cpu state causing potentially undefined behaviour
    unsafe fn write(write: R){
        let high = write.bits().0;
        let low = write.bits().1;


        core::arch::asm!(
            "wrmsr",
            in("ecx") Self::MSR_ADDR,
            in("eax") low,
            in("edx") high
        )

    }

    /// Checks `cpuid` for available bits and their read/write/lock status
    fn availability() -> MsrAvailability;
}


/// This trait is used for the
pub trait MsrFlags{
    /// This function converts 2 u32's to a u64
    ///
    /// A struct may use the result to generate its output
    fn bitfield(high: u32, low: u32) -> u64{
        let mut high = high as u64;
        let low = low as u64;
        high <<= 32;
        high |= low;
        high
    }

    /// Returns self as `(high, low)` bits;
    ///
    /// note: that high is used in the `edx` register and low is used in `eax`
    fn bits(&self) -> (u32,u32);

    fn split_bits(big: u64) -> (u32,u32){
        let low = big as u32;
        let high = (big >> 32 ) as u32;
        (high,low)
    }

    /// Returns Self from high and low bits
    fn new(high: u32, low: u32) -> Self;
}


/// This struct contains information about the read/write status of
/// MSRs and should not be modified after being created
///
/// All bits marked as `Write` are also `read`.
///
/// Some states may change after an MSR is written
#[derive(Clone)]
pub struct MsrAvailability ([MsrReadWrite; 64]);


#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MsrReadWrite{
    Read,
    Write,
    Reserved,
}

impl MsrAvailability{
    pub fn new(state: MsrReadWrite) -> Self{
        Self([state; 64])
    }

    pub fn set_bit(&mut self, bit: usize, state: MsrReadWrite) {
        self.0[bit] = state
    }

    pub fn get_bit(&self, bit: usize) -> MsrReadWrite {
        self.0[bit]
    }
}

/// calls cpuid_lookup_reg and returns the state for the given bit in the specified register
pub fn cpuid_lookup_bit(reg: CpuidRegister, leaf: u32, sub_leaf: u32, bit: u8) -> bool{
    let cpuid_dat = cpuid_lookup_reg(reg, leaf, sub_leaf);

    let masked = cpuid_dat & (1 << bit);
    return if masked == 0 {
        false
    } else {
        true
    }
}

/// This function calls the `cpuid` instruction with the given leaf and sub-leaf
/// and returns the data specified with reg.
///
/// note that sub-leaf is always set but is ignored if a leaf without
/// sub-leaves is selected, in this case it may be set to anything.
#[allow(unused_assignments)]
pub fn cpuid_lookup_reg(reg: CpuidRegister, mut leaf: u32, mut sub_leaf: u32) -> u32 {


    let mut edx: u32;
    let mut ebx: u32;

    unsafe{
        core::arch::asm!(
        "xchg ebx,{0:e}", // wow thanks rust docs for telling me about {:e}
        "cpuid",
        "mov {0:e}, ebx",
        "xchg ebx,{0:e}",
        out(reg) ebx,
        inout("eax") leaf,
        inout("ecx") sub_leaf,
        out("edx") edx,
        )
    }
    return match reg {
        CpuidRegister::eax => leaf, // this is eax at this point
        CpuidRegister::ebx => ebx,
        CpuidRegister::ecx => sub_leaf, // this is ecx at this point
        CpuidRegister::edx => edx,
    }
}

#[allow(non_camel_case_types)]
pub enum CpuidRegister{
    eax,
    ebx,
    ecx,
    edx,
}