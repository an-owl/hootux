//! This module contains registers marked as Architectural by the [Intel software developers manual volume 4](https://cdrdv2.intel.com/v1/dl/getContent/671098)
//! all MSR names in this module the their prefix "IA32_" omitted and written in camel case
//! i.e "IA32_TIME_STAMP_COUNTER" becomes "TimeStampCounter"
//!
//! Any modifications to an instance of `impl MsrFlags` must be manually written back the the register
//!
//! Not all MSRs are currently implemented

use crate::MsrReadWrite::Reserved;
use crate::*;
use bitflags::bitflags;

/// IA_32_TIME_STAMP_COUNTER see <https://www.intel.com/content/dam/develop/public/us/en/documents/335592-sdm-vol-4.pdf>
/// for more info
pub struct TimeStampCounter {}
#[derive(Copy, Clone)]

/// Contains a value for TimeStampCounterValue
///
/// All values for this register are valid
pub struct TimeStampCounterValue {
    pub value: u64, //pub because it doesnt really need protection
}

impl Msr<TimeStampCounterValue> for TimeStampCounter {
    const MSR_ADDR: u32 = 0x10;
    fn availability() -> MsrAvailability {
        return if cpuid_lookup_bit(CpuidRegister::edx, 1, 0, 4) {
            MsrAvailability::new(MsrReadWrite::Write)
        } else {
            MsrAvailability::new(MsrReadWrite::Reserved)
        };
    }
}

impl MsrFlags for TimeStampCounterValue {
    fn bits(&self) -> (u32, u32) {
        let low = self.value as u32;
        let high = (self.value >> 32) as u32;

        return (high, low);
    }

    fn new(high: u32, low: u32) -> Self {
        let total = Self::bitfield(high, low);
        Self { value: total }
    }
}
/// This for whatever reason does not work.
/// I would however like to know what it does work on
pub struct SpecCtrl {}

bitflags! {
    /// This for whatever reason does not work
    pub struct SpecCtrlFlags: u64 {
        const INDIRECT_BRANCH_RESTRICTED_SPECULATION =      1 << 0;
        const SINGLE_THREAD_INTIRECT_BRANCH_PREDICTORS =    1 << 1;
        const SPECULATIVE_STORE_BYPASS_DISABLE =            1 << 2;
    }
}

impl MsrFlags for SpecCtrlFlags {
    fn bits(&self) -> (u32, u32) {
        let low = self.bits as u32;
        let high = (self.bits >> 32) as u32;
        (high, low)
    }

    fn new(high: u32, low: u32) -> Self {
        Self {
            bits: Self::bitfield(high, low),
        }
    }
}

impl Msr<SpecCtrlFlags> for SpecCtrl {
    const MSR_ADDR: u32 = 0x48;

    fn availability() -> MsrAvailability {
        let mut bits = MsrAvailability::new(Reserved);

        if cpuid_lookup_bit(CpuidRegister::edx, 7, 0, 26) {
            bits.0[0] = MsrReadWrite::Write;
        }

        if cpuid_lookup_bit(CpuidRegister::edx, 7, 0, 27) {
            bits.0[1] = MsrReadWrite::Write;
        }

        if cpuid_lookup_bit(CpuidRegister::edx, 7, 0, 31) {
            bits.0[2] = MsrReadWrite::Write;
        }

        bits
    }
}

/// This register contains information about the Apic including its base physical address
pub struct ApicBase {}

bitflags! {
    /// see [ApicBase]
    pub struct ApicBaseData: u64{
        const BSP_FLAG =            1 << 8;
        const X2APIC_ENABLE_MODE =  1 << 10;
        const APIC_GLOBAL_ENABLE =  1 << 11;
    }
}

impl ApicBaseData {
    /// This returns the apic base address
    pub fn get_apic_base_addr(&self) -> u64 {
        self.bits & !Self::all().bits
    }

    /// This returns the apic base address
    pub fn set_apic_base_addr(&mut self, base_addr: u64) {
        assert!(base_addr < *MAXPHYADDR_MASK);
        let mut mask = *MAXPHYADDR_MASK >> 12 ;
        mask = mask << 12;
        self.bits &= !mask; // clear bits in address range
        let base_addr = (base_addr >> 12);
        self.bits |= base_addr << 12;
    }
}

impl MsrFlags for ApicBaseData {
    fn bits(&self) -> (u32, u32) {
        Self::split_bits(self.bits)
    }

    fn new(high: u32, low: u32) -> Self {
        Self {
            bits: Self::bitfield(high, low),
        }
    }
}

impl Msr<ApicBaseData> for ApicBase {
    const MSR_ADDR: u32 = 0x1b;

    /// While `BSP_FLAG` is flagged as write that's probably not a good idea
    fn availability() -> MsrAvailability {
        let cpuid = raw_cpuid::CpuId::new();
        let feature = cpuid
            .get_feature_info()
            .expect("unable to get feature info");

        let mut bits = MsrAvailability::new(MsrReadWrite::Reserved);
        if ((feature.family_id() == 6) && (feature.model_id() >= 1)) || feature.family_id() > 6 {
            bits.0[8] = MsrReadWrite::Write;
            bits.0[11..*MAXPHYADDR as usize - 1].fill_with(|| MsrReadWrite::Write);
            if ((feature.family_id() == 6) && (feature.model_id() >= 0x1a))
                || feature.family_id() > 6
            {
                bits.0[10] = MsrReadWrite::Write;
            }
        }

        bits
    }
}
