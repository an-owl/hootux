use core::cmp::Ordering;
use core::fmt::{Debug, Display};
use core::ops::{Add, BitAnd, BitOr, Not, Sub};


const fn assert_wf_bool(value: bool) -> usize {
    if value {
        1
    } else {
        panic!()
    }
}

/// Contains a free list which is used to store large regions of memory in an optimized layout.
///
/// Allocations of the size which are expected to hit this struct are expected to be infrequent.
/// For this reason speed is not a high priority of this interface.
///
/// BS must always be a power of two
// BS is u32 because u16 is too small. If (god knows why) an AllocAddressType smaller than u32 is
// needed this entire struct sound be excluded from compilation
#[allow(private_bounds)]
#[derive(Debug)]
pub(super) struct HighOrderAlloc<T: AllocAddressType, const BS: u32>
    where
    [(); assert_wf_bool(BS.is_power_of_two())]:
{
    free_list: alloc::vec::Vec<FreeMem<T, BS>>,
}

#[allow(private_bounds)]
impl<T: AllocAddressType, const BS: u32> HighOrderAlloc<T, BS> where [(); assert_wf_bool(BS.is_power_of_two())]: {
    pub(super) const fn new() -> Self {
        Self {
            free_list: alloc::vec::Vec::new(),
        }
    }

    pub(super) fn free(&mut self, region: FreeMem<T, BS>) -> Result<(), ()> {
        #[cfg(feature = "alloc-debug-serial")]
        crate::serial_println!(
            "{}::free(region: {:x?})",
            core::any::type_name::<Self>(),
            region
        );

        // this should not panic during normal operation.
        let r = self.free_list.binary_search(&region);
        let i = r.expect_err("Duplicate FreeMem found");

        if let Some(b) = self.free_list.get_mut(i) {
            if b.merge(region).is_none() {
                #[cfg(feature = "alloc-debug-serial")]
                crate::serial_println!("{:x?}", self.free_list);
                return Ok(());
            }
        }
        if let Some(b) = self.free_list.get_mut(i - 1) {
            if b.merge(region).is_none() {
                #[cfg(feature = "alloc-debug-serial")]
                crate::serial_println!("{:x?}", self.free_list);
                return Ok(());
            }
        }
        if let Some(b) = self.free_list.get_mut(i + 1) {
            if b.merge(region).is_none() {
                #[cfg(feature = "alloc-debug-serial")]
                crate::serial_println!("{:x?}", self.free_list);
                return Ok(());
            }
        }

        self.free_list.insert(i, region);
        #[cfg(feature = "alloc-debug-serial")]
        crate::serial_println!("{:x?}", self.free_list);
        Ok(())
    }

    /// Attempts to allocate memory using a worst fit algorithm
    ///
    /// This fn will return a region which can be used for `layout` but the size of the returned region may vary.
    pub(super) fn allocate(&mut self, layout: core::alloc::Layout) -> Option<FreeMem<T, BS>> {
        #[cfg(feature = "alloc-debug-serial")]
        {
            crate::serial_println!(
                "HighOrderAlloc<{},{}>::allocate()",
                core::any::type_name::<T>(),
                BS
            );
            crate::serial_println!("list on entry: {:?}", self.free_list);
        }
        // always prefer clean blocks.
        let mut is_clean = false;
        // for unclean blocks this is the size of the smallest block
        // for clean blocks this is the remaining size of the block
        // smaller is better, 0 is perfect
        let mut score = None;
        let mut index = None;

        for (i, b) in self.free_list.iter().enumerate() {
            if b.chk_fit(layout) {
                let (c, s) = b.alloc_score(layout);

                match (c, is_clean) {
                    (true, false) => {
                        is_clean = true;
                        index = Some(i);
                        score = Some(s);
                    }

                    // always prefer clean blocks.
                    (false, true) => {}

                    // Only triggers when both bools are equal.
                    // This doesnt use a match guard because then i need a `_ => unreachable!()`
                    // if they are the same cleanliness then just compare scores.
                    _ => {
                        if s < score.unwrap() {
                            score = Some(s);
                            index = Some(i);
                        }
                    }
                }

                // if score is perfect break and use this block
                if let Some(n) = score {
                    if n == T::from_u32(0) {
                        break;
                    }
                }
            }
        }
        let (ret, free) = self.free_list.remove(index?).force_split_to_layout(layout);
        if let Some(f) = free {
            self.free_list.insert(
                self.free_list
                    .binary_search(&f)
                    .expect_err("Duplicate memory region"),
                f,
            );
        }
        ret
    }
}

#[derive(Debug, Copy, Clone)]
pub(super) struct FreeMem<T: AllocAddressType, const BS: u32> {
    start: T,
    len: T,
}

impl<T: AllocAddressType, const BS: u32> FreeMem<T, BS> {
    pub(super) fn new(addr: T, len: T) -> Option<Self> {
        // ensures that BS is always pow2.
        // I can't really put this anywhere else.
        let min = T::from_u32(BS);

        // check size/align
        if len < min {
            return None;
        } else if addr & (min - T::from_u32(1)) != T::from_u32(0) {
            return None;
        }

        Some(Self { start: addr, len })
    }

    /// Returns `(start,len)`
    pub fn as_raw(self) -> (T, T) {
        (self.start, self.len)
    }

    /// Returns the value of the next byte
    fn end(&self) -> T {
        self.start + self.len
    }

    /// Attempts to merge `self` with `other`, returns `other` if this fails.
    fn merge(&mut self, mut other: Self) -> Option<Self> {
        if self.end() == other.start {
            self.len = self.len + other.len;
            None
        } else if other.end() == self.start {
            other.len = other.len + self.len;
            *self = other;
            None
        } else {
            Some(other)
        }
    }

    /// Splits self ensuring the new size of self is `new_size` and the remaining size is in the
    /// returned value.
    ///
    /// # Panics
    ///
    /// This fn will panic if
    ///
    ///  - `new_size` is not aligned to `BS`
    ///  - `new_size >= self.len`
    ///  - `new_size == 0`
    fn split(&mut self, new_size: T) -> Self {
        assert_ne!(self.start - new_size, T::from_u32(0)); // will panic if self.start < new_size
        assert_eq!(new_size & T::from_u32(BS - 1), T::from_u32(0));

        let b2s = self.len + new_size;
        let diff = self.len - new_size;
        let ret = Self::new(b2s, diff).expect("Failed to split B");

        self.len = new_size;
        ret
    }

    /// Returns whether self can be broken into a block which fits `layout`.
    fn chk_fit(&self, layout: core::alloc::Layout) -> bool {
        let size = T::from_usize(layout.size());
        let align = T::from_usize(layout.size());

        let use_start;
        if self.start & (align - T::from_u32(1)) == T::from_u32(0) {
            use_start = self.start;
        } else {
            use_start = self.start & !(align - T::from_u32(1));
        }

        self.end() - use_start > size
    }

    /// Checks if `self` can be cleanly split into to allocate `layout`
    fn chk_clean(&self, layout: core::alloc::Layout) -> bool {
        let size = T::from_usize(layout.size());
        let align = T::from_usize(layout.size());

        if size > self.len {
            return false;
        } else if self.start & align - T::from_u32(1) == T::from_u32(0) {
            true
        } else if (self.end() - size) & (align - T::from_u32(1)) == T::from_u32(0) {
            true
        } else {
            false
        }
    }

    /// Attempts to split self, returning the segment split from self if any.
    ///
    /// This fn will only return `Some(_)` if a block can be split cleanly from either end.
    fn try_split_to_layout(&mut self, layout: core::alloc::Layout) -> Option<Self> {
        let size = T::from_usize(layout.size());
        let align = T::from_usize(layout.align());
        if !self.chk_fit(layout) {
            return None;
        } else if self.len < size {
            return None;
        }

        if self.start & (T::from_usize(layout.align()) - T::from_u32(1)) == T::from_u32(0) {
            let mut t = self.split(size);
            core::mem::swap(&mut t, self);
            return Some(t);
        } else if (self.end() - size) & (align - T::from_u32(1)) == T::from_u32(0) {
            Some(self.split(self.len - size))
        } else {
            None
        }
    }

    /// Forcibly splits `self` multiple times returning an allocated block and a remaining block
    /// This fn will call [Self::try_split_to_layout] to check that self must return the unused block
    ///
    /// The returned tuple contains `(allocated, remaining)` the `remaining` block should be added to the free list.
    fn force_split_to_layout(
        &mut self,
        layout: core::alloc::Layout,
    ) -> (Option<Self>, Option<Self>) {
        let size = T::from_usize(layout.size());
        let align = T::from_usize(layout.size());

        if !self.chk_fit(layout) {
            return (None, None);
        } else if self.len < size {
            return (None, None);
        }

        // ensure that a clean split cannot be done
        if let Some(b) = self.try_split_to_layout(layout) {
            return (Some(b), None);
        }

        // check if breaking from the top or bottom is best
        // best being leaving the smallest and largest block possible.
        let lbs = (self.start & align) + align; // Low Block Start
        let low_block_size = self.start - lbs;

        let hbs = (self.end() - size) & !align;
        let high_block_size = self.end() - (hbs + size);

        // prefer low block because it uses one less op
        if high_block_size > low_block_size {
            let mut rb = self.split(low_block_size);
            let nb = rb.split(size);
            (Some(rb), Some(nb))
        } else {
            let mut rb = self.split(self.start - hbs);
            let nb = rb.split(size);
            (Some(rb), Some(nb))
        }
    }

    /// Returns the score of the block against the layout.
    ///
    /// # Panics
    ///
    /// This fn will panic if `!self.chk_fit(layout)`
    fn alloc_score(&self, layout: core::alloc::Layout) -> (bool, T) {
        if !self.chk_fit(layout) {
            panic!("Attempted to score block which does not fit");
        }
        let mut c = self.clone();

        let clean = self.chk_clean(layout);
        return if clean {
            // never returns None if `self.chk_clean == true`
            c.try_split_to_layout(layout).unwrap();
            (clean, c.len)
        } else {
            if let (_, Some(b)) = c.force_split_to_layout(layout) {
                (clean, c.len.min(b.len))
            } else {
                // force_split_to_layout only returns (_,None) when `chk_clean == false`
                unreachable!();
            }
        };
    }
}

impl<T: AllocAddressType, const BS: u32> PartialEq<Self> for FreeMem<T, BS> {
    fn eq(&self, other: &Self) -> bool {
        self.start.eq(&other.start)
    }
}

impl<T: AllocAddressType, const BS: u32> PartialOrd for FreeMem<T, BS> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.start.partial_cmp(&other.start)
    }
}

impl<T: AllocAddressType, const BS: u32> Ord for FreeMem<T, BS> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.start.cmp(&other.start)
    }
}

impl<T: AllocAddressType, const BS: u32> Eq for FreeMem<T, BS> {}

/// This is a helper trait to help with not typing out all the required traits for everything that needs this.
pub(super) trait AllocAddressType:
    BitAnd<Output = Self>
    + BitOr<Output = Self>
    + Not<Output = Self>
    + Add<Output = Self>
    + Sub<Output = Self>
    + Display
    + Eq
    + Ord
    + Debug
    + Copy
    + Sized
{
    fn is_power_of_two(&self) -> bool;
    fn from_u32(origin: u32) -> Self
    where
        Self: Sized;
    fn from_usize(origin: usize) -> Self
    where
        Self: Sized;
}

impl AllocAddressType for usize {
    fn is_power_of_two(&self) -> bool {
        usize::is_power_of_two(*self)
    }

    fn from_u32(origin: u32) -> Self
    where
        Self: Sized,
    {
        origin as usize
    }

    fn from_usize(origin: usize) -> Self
    where
        Self: Sized,
    {
        origin
    }
}

impl AllocAddressType for u64 {
    fn is_power_of_two(&self) -> bool {
        u64::is_power_of_two(*self)
    }

    fn from_u32(origin: u32) -> Self
    where
        Self: Sized,
    {
        origin.into()
    }
    fn from_usize(origin: usize) -> Self
    where
        Self: Sized,
    {
        origin as u64
    }
}

impl<const BS: u32> FreeMem<usize, BS> {
    pub unsafe fn into_ptr(self) -> *mut [u8] {
        // SAFETY: safety rules here should be kind of obvious. dont play with fire kids
        core::slice::from_raw_parts_mut(self.start as *mut u8, self.len)
    }
}
