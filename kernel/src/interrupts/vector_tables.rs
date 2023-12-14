pub static IHR: HandleRegistry = HandleRegistry::new();

#[thread_local]
pub(super) static mut INT_LOG: InterruptLog = InterruptLog::new();

/// The InterruptLog is a thread local structure used for counting the number of interrupts that
/// have occurred  on each cpu. When an interrupt occurs its vector within log should be incremented.
/// No other software should modify InterruptLog. It is made thread local to avoid the use of mutexes
/// which may in some cases cause a deadlock, this also reduces access time helping to
/// remove interrupt overhead.
///
/// When the values within this are read and are lower than when they were read previously this can
/// be guaranteed to either a hardware fault or an overflow and should be treated as the latter,
/// however is likely the former.
pub struct InterruptLog {
    log: [usize; 256],
}

impl InterruptLog {
    pub const fn new() -> Self {
        Self { log: [0; 256] }
    }

    /// Logs an interrupt into internal storage at the given vector
    pub(super) fn log(&mut self, vector: u8) {
        self.log[vector as usize] = self.log[vector as usize].wrapping_add(1);
    }

    /// Returns a copy of the data contained within the given vector
    pub fn fetch_vec(&self, vector: u8) -> usize {
        self.log[vector as usize]
    }

    pub fn fetch_all(&self) -> [usize; 256] {
        self.log.clone()
    }
}

/// Handle registry stores interrupt handlers. it is used to serve multiple purposes that can't be
/// handled by using the idt directly.
///
/// It enables the use of interrupt stubs that can handle critical functionality such as logging
/// itself in the interrupt log. Interrupt stubs also allow interrupt vectors without a proper
/// handler to be called without triggering a double fault.
///
/// It also enables interrupt handlers to be modified at runtime without disabling interrupts or
/// risking a double fault.
pub struct HandleRegistry {
    // This has a special use. this must be locked and freed manually to prevent multiple CPU's from
    // modifying the struct at the same time. but allowing interrupts to access `self.arr` at the
    // same time. A normal mutex cannot be used because the whole IHR cannot be behind a mutex and
    // mutex relies on returning a struct that implements drop to free the mutex.
    lock: core::sync::atomic::AtomicBool,
    arr: [spin::RwLock<InterruptHandleContainer>; 256],
}

impl HandleRegistry {
    /// Creates a new instance of Self where all entries are `None`
    pub const fn new() -> Self {
        Self {
            lock: core::sync::atomic::AtomicBool::new(false),
            arr: [const { spin::RwLock::new(InterruptHandleContainer::Empty) }; 256],
        }
    }

    /// Locks Self spinning if necessary should be used when a CPU requires access to the IHR except
    /// for calling an interrupt
    ///
    /// # Caution
    ///
    /// If lock is called [Self::free] must be called. failure to do so may cause a spinlock
    pub(crate) fn lock(&self) {
        while let Err(_) = self.lock.compare_exchange_weak(
            false,
            true,
            atomic::Ordering::Acquire,
            atomic::Ordering::Relaxed,
        ) {
            core::hint::spin_loop()
        }
    }

    pub(crate) fn free(&self) {
        self.lock.store(false, atomic::Ordering::Release);
    }

    /// Sets the given vector to reserved
    pub fn reserve(&self, vector: u8) -> Result<(), ()> {
        let mut vec = self.arr[vector as usize].write();
        return if let InterruptHandleContainer::Empty = *vec {
            *vec = InterruptHandleContainer::Reserved;
            Ok(())
        } else {
            Err(())
        };
    }

    /// Sets interrupt handler for the given vector. If the given vector already contains a handler
    /// this will return `Err()`, in this case you must run [HandleRegistry::unset]
    ///
    /// # Deadlocks
    ///
    /// This can potentially cause a deadlock. If an interrupt calling this vector is handled on the
    /// CPU which has called this fn this will cause deadlock
    pub(super) fn set(&self, vector: u8, handle: InterruptHandleContainer) -> Result<(), ()> {
        match &mut *self.arr[vector as usize].write() {
            InterruptHandleContainer::SpecialHandle(_) => Err(()),
            InterruptHandleContainer::Generic(_) => Err(()),
            h => {
                *h = handle;
                Ok(())
            }
        }
    }

    /// Frees the given IRQ
    ///
    /// # Safety
    ///
    /// The caller must ensure that the IRQ will not be raised after this fn is called.
    pub(super) unsafe fn free_irq(&self, vector: super::InterruptIndex) -> Result<(), ()> {
        let mut e = InterruptHandleContainer::Empty;
        core::mem::swap(&mut *self.arr[vector.as_usize()].write(), &mut e);
        match e {
            InterruptHandleContainer::Empty => Err(()),
            InterruptHandleContainer::Reserved => Ok(()),
            InterruptHandleContainer::SpecialHandle(_) => Ok(()),
            InterruptHandleContainer::Generic(_) => Ok(()),
            InterruptHandleContainer::HighPerfCascading(_) => Ok(()),
        }
    }

    /// Unsets the given vector handler.
    ///
    /// # Safety
    ///
    /// This fn is unsafe because the caller must ensure that no interrupts will ge generated on
    /// this interrupt.
    pub(super) unsafe fn unset(&self, vector: u8) {
        *self.arr[vector as usize].write() = InterruptHandleContainer::Empty
    }

    /// Returns a reference to to the given vector handler.
    pub(crate) fn get(&self, vector: u8) -> &spin::RwLock<InterruptHandleContainer> {
        &self.arr[vector as usize]
    }

    /// Gets the number of free IRQs
    pub(crate) fn get_free(&self) -> u8 {
        let mut count = 0;
        for i in &self.arr {
            if let InterruptHandleContainer::Empty = *i.read() {
                count += 1;
            }
        }
        count
    }

    /// Locates a contiguous region of free vectors and reserves them.
    ///
    /// On success returns a vector number. `ret..ret+req` vectors are guaranteed to be unused.
    /// On fail returns the highest number contiguous vectors found
    pub(crate) fn reserve_contiguous(&self, start: u8, req: u8) -> Result<u8, u8> {
        let mut max = 0;
        let mut found_start = None;
        let mut found = 0;

        let start = start.max(32);

        for (i, v) in self.arr.iter().enumerate().skip(start as usize) {
            match *v.read() {
                InterruptHandleContainer::Empty if found_start.is_none() => {
                    found_start = Some(i);
                    found = 1;
                }
                InterruptHandleContainer::Empty => {
                    found += 1;
                    if found == req {
                        break;
                    }
                }

                _ => {
                    max = found.max(max);
                    found_start = None;
                    found = 0;
                }
            }
        }

        if found < req {
            max = max.max(found);
            return Err(max);
        }

        let vec = found_start.ok_or(max.max(found))?;

        for i in &self.arr[vec..vec + req as usize] {
            *i.write() = InterruptHandleContainer::Reserved
        }

        Ok(vec as u8)
    }
}

// SAFETY: HandleRegistry handles its own thread synchronization
unsafe impl Sync for HandleRegistry {}

/// Allocates a special interrupt handler for `irq` which will call `handle`.
///
/// Returns `Err(()) if the given interrupt handler is already allocated.
#[cold]
pub(crate) fn alloc_irq_special(irq: u8, handle: fn()) -> Result<(), ()> {
    let mut r = IHR.get(irq).write();
    match *r {
        InterruptHandleContainer::Empty => {
            *r = InterruptHandleContainer::SpecialHandle(handle);
            Ok(())
        }
        InterruptHandleContainer::Reserved => {
            *r = InterruptHandleContainer::SpecialHandle(handle);
            Ok(())
        }
        _ => Err(()),
    }
}

/// This struct is for Interrupts used to signal drivers. When an interrupt of this type is called,
/// the kernel will wake the specified task and push the vector number onto its buffer.
///
/// A driver is responsible for managing its own buffer and it should ensure that it has enough space.
/// If a buffer is full the message will be discarded.
///
/// If this IRQ uses interrupt Coalescing the driver must handle this properly. Note that a message
/// may occur more than once.
pub struct InterruptHandle {
    waker: futures_util::task::AtomicWaker,
    queue: crate::task::InterruptQueue,
    vector: crate::task::InterruptMessage,
}

impl InterruptHandle {
    /// Creates a new instance of `Self`
    ///
    /// # Safety
    ///
    /// This fn is unsafe because the caller must ensure that the [crate::task::TaskId] is handed by `queue`
    pub const unsafe fn new(queue: crate::task::InterruptQueue, message: u64) -> Self {
        Self {
            waker: futures_util::task::AtomicWaker::new(),
            queue,
            vector: crate::task::InterruptMessage(message),
        }
    }

    /// Calls the handler. If the buffer is full then the message will be discarded
    pub fn call(&self) {
        // as docs mention: on fail message is discarded
        self.queue.push(self.vector).unwrap_or(());
        self.waker.wake();

        // SAFETY: This is an interrupt handler
        unsafe { super::apic::apic_eoi() };
    }

    pub fn register(&self, waker: &core::task::Waker) {
        self.waker.register(&waker)
    }
}

/// Indicates the current state of the IRQ and the type of handler it uses.
pub enum InterruptHandleContainer {
    Empty,
    /// Reserved IRQs are specially requested, These should not be modified except by the caller
    /// that reserved them
    Reserved,
    /// Special handles are for interrupt handlers that must take special actions when they are requested
    SpecialHandle(fn()),
    /// Generic handlers will wake a kernel task and push a vector number onto the tasks work queue
    Generic(InterruptHandle),
    HighPerfCascading(alloc::vec::Vec<alloc::boxed::Box<dyn Fn()>>),
}

impl InterruptHandleContainer {
    pub(super) fn callable(&self) -> Option<&Self> {
        match self {
            InterruptHandleContainer::Empty => None,
            InterruptHandleContainer::Reserved => None,
            InterruptHandleContainer::SpecialHandle(_) => Some(self),
            InterruptHandleContainer::Generic(_) => Some(self),
            InterruptHandleContainer::HighPerfCascading(_) => Some(self),
        }
    }

    pub(super) fn call(&self) {
        match self {
            InterruptHandleContainer::SpecialHandle(h) => h(),
            InterruptHandleContainer::Generic(h) => h.call(),
            InterruptHandleContainer::HighPerfCascading(v) => {
                for i in v {
                    i()
                }
            }
            _ => {}
        }
    }
}
