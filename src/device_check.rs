use core::fmt::Formatter;

/// Enables simple hardware checking
/// Checks type U for the presence of T
pub(crate) struct MaybeExists<T,U:DeviceCheck>{
    inner: T,
    check: U
}

impl<T,U:DeviceCheck> MaybeExists<T,U> {
    pub fn try_fetch(&self) -> Option<&T>{
        if U::exists() {
            Some(&self.inner)
        } else { None }
    }

    pub fn try_fetch_mut(&mut self) -> Option<&mut T> {
        if U::exists() {
            Some(&mut self.inner)
        } else { None }
    }
}

impl<T,U:DeviceCheck> core::fmt::Debug for MaybeExists<T,U>
    where T: core::fmt::Debug
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        if U::exists() {
            write!(f,"{:?}",self.inner)
        } else {
            write!(f, "Not Present")
        }
    }
}

impl<T,U:DeviceCheck> core::fmt::Display for MaybeExists<T,U>
    where T: core::fmt::Debug
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        if U::exists() {
            write!(f,"{:?}",self.inner)
        } else {
            write!(f, "Not Present")
        }
    }
}

pub(crate) trait DeviceCheck{
    fn exists() -> bool;
}