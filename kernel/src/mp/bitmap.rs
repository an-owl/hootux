
pub struct CpuBMap {
    // This uses a Vec to avoid th issue linux had (has?) with systems with more thant 256 CPUs
    // u64 is the smallest size we can allocate so this doesn't waste space
    map: spin::RwLock<alloc::vec::Vec<core::sync::atomic::AtomicUsize>>,
}

impl CpuBMap {
    pub const fn new() -> Self {
        Self {
            map: spin::RwLock::new(alloc::vec::Vec::new())
        }
    }

    /// Sets the bit representing the current CPU
    pub fn set(&self) {
        self.set_bit(super::who_am_i())
    }

    /// Sets the bit for the specified CPU
    pub fn set_bit(&self, cpu: super::CpuIndex) {
        let cpu = cpu as usize;
        let offset = cpu % usize::BITS as usize;
        let index = cpu / usize::BITS as usize;

        let l = self.map.upgradeable_read();
        if l.len() < index {
            let mut l = l.upgrade();
            l.resize_with(index, || Default::default());
            l[index].fetch_or(1 << offset, atomic::Ordering::Relaxed);
        } else {
            l[index].fetch_or(1 << offset, atomic::Ordering::Relaxed);
        }
    }

    pub fn clear(&self) {
        self.clear_bit(super::who_am_i())
    }

    pub fn clear_bit(&self, cpu: super::CpuIndex) {
        let cpu = cpu as usize;
        let offset = cpu % usize::BITS as usize;
        let index = cpu / usize::BITS as usize;

        let l = self.map.upgradeable_read();
        if l.len() < index {
            let mut l = l.upgrade();
            l.resize_with(index, || Default::default());
            l[index].fetch_and(!(1 << offset), atomic::Ordering::Relaxed);
        } else {
            l[index].fetch_or(!(1 << offset), atomic::Ordering::Relaxed);
        }
    }

    pub fn get(&self, cpu: super::CpuIndex) -> bool {
        if let Some(u) = self.map.read().get(cpu as usize/u64::BITS as usize) {
            let t = u.load(atomic::Ordering::Relaxed);
            // check if bit is set
            t & 1 << cpu % usize::BITS == 1
        } else {
            false
        }
    }

    pub const fn iter(&self) -> CpuBitmapIterator {
        CpuBitmapIterator {
            map: self,
            last: None
        }
    }

    /// Returns the number of all the CPUs which are set.
    /// This will return None if no CPUs are set
    pub fn count(&self) -> Option<super::CpuCount> {
        let mut count = 0;
        for i in self.map.read().iter() {
            let u = i.load(atomic::Ordering::Relaxed);
            count += u.count_ones();
        }

        count.try_into().ok()
    }

    /// Returns the contents of `self`, clears the contents of `self`.
    fn take(&mut self) -> Self {
        let mut n = Self::new();
        n.map.write().resize_with(self.map.read().len(),|| Default::default());
        core::mem::swap(self,&mut n);
        n
    }
}

struct CpuBitmapIterator<'a> {
    map: &'a CpuBMap,
    last: Option<super::CpuIndex>
}

impl<'a> Iterator for CpuBitmapIterator<'a> {
    type Item = super::CpuIndex;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(n) = self.last {
            let mut index = n / usize::BITS;
            let offset = n % usize::BITS;
            let arr = self.map.map.read();
            // check current index

            if offset > 0 {
                let mut u = arr[index as usize].load(atomic::Ordering::Relaxed);
                u &= !(1 << offset) - 1;
                let fo = u.trailing_zeros();
                if fo != usize::BITS {
                    self.last = Some(index * usize::BITS + fo);
                    return self.last
                } else {
                    index += 1;
                }
            }

            for (i,n) in arr.iter().enumerate().skip(index as usize) {
                let u = n.load(atomic::Ordering::Relaxed);
                let fo = u.trailing_zeros();
                if fo != usize::BITS {
                    self.last = Some((i * usize::BITS as usize + fo as usize) as super::CpuIndex);
                    return  self.last
                }
            }
            return None

        } else {
            if self.map.get(0) {
                self.last = Some(0);
               return Some(0);
            } else {
                self.last = Some(0);
                self.next()
            }
        }
    }
}