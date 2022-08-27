use core::fmt::Formatter;

/// Enables simple hardware checking
pub(crate) struct MaybeExists<T,const CHECK: CheckType>{
    inner: T
}

impl<T,const CHECK: CheckType> MaybeExists<T, CHECK> {
    pub fn try_fetch(&self) -> Option<&T>{
        if CHECK.exists() {
            Some(&self.inner)
        } else { None }
    }

    pub fn try_fetch_mut(&mut self) -> Option<&mut T> {
        if CHECK.exists() {
            Some(&mut self.inner)
        } else { None }
    }
}

impl<T,const CHECK: CheckType> core::fmt::Debug for MaybeExists<T,CHECK>
    where T: core::fmt::Debug
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        if CHECK.exists() {
            write!(f,"{:?}",self.inner)
        } else {
            write!(f, "Not Present")
        }
    }
}

impl<T,const CHECK: CheckType> core::fmt::Display for MaybeExists<T,CHECK>
    where T: core::fmt::Debug
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        if CHECK.exists() {
            write!(f,"{:?}",self.inner)
        } else {
            write!(f, "Not Present")
        }
    }
}

#[derive(Eq, PartialEq,Debug)]
#[non_exhaustive]
pub(crate) enum CheckType{
    ApicThermalInterrupt,
    ApicPerformanceInterrupt,
}

impl CheckType {
    fn exists(&self) -> bool {
        return match self {
            CheckType::ApicThermalInterrupt => {
                raw_cpuid::CpuId::new().get_feature_info().unwrap().has_acpi()
            }
            CheckType::ApicPerformanceInterrupt => {
                return if let Some(_) = raw_cpuid::CpuId::new().get_performance_monitoring_info() {
                    true
                } else { false }
            }
        }
    }
}