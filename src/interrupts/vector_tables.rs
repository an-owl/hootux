
pub static IHR: HandleRegistry = HandleRegistry::new();


/// The InterruptLog is a thread local structure used for counting the number of interrupts that
/// have occurred  on each cpu. When an interrupt occurs its vector within log should be incremented.
/// No other software should modify InterruptLog. It is made thread local to avoid the use of mutexes
/// which may in some cases cause a deadlock, this also reduces access time helping to
/// remove interrupt overhead.
///
/// When the values within this are read and are lower than when they were read previously this can
/// be guaranteed to either a hardware fault or an overflow and should be treated as the latter,
/// however is likely the former.
pub struct InterruptLog{
    log: [usize;256]
}

impl InterruptLog {
    pub const fn new() -> Self {
        Self{
            log: [0;256]
        }
    }

    /// Logs an interrupt into internal storage at the given vector
    pub(super) fn log(&mut self, vector: u8) {
        self.log[vector as usize] = self.log[vector as usize].wrapping_add(1);
    }

    /// Returns a copy of the data contained within the given vector
    pub fn fetch_vec(&self,vector: u8) -> usize {
        self.log[vector as usize]
    }

    pub fn fetch_all(&self) -> [usize;256]{
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
    arr: [spin::RwLock<Option<fn()>>;256]
}



impl HandleRegistry {
    /// Creates a new instance of Self where all entries are `None`
    pub const fn new() -> Self {
        Self{
            // this is stupid
            arr: [ const { spin::RwLock::new(None) }; 256 ]
        }
    }

    /// Sets interrupt handler for the given vector. If the given vector already contains a handler
    /// fn this will return `Err()`, in this case you must run [HandleRegistry::unset]
    ///
    /// #Saftey
    /// This function is unsafe because the caller must ensure that the given vector is correct.
    ///
    /// `f` must also correctly handle the given interrupt and deliver EOI, failure to do so may
    /// prevent the device form working correctly until it is reset
    pub unsafe fn set(&self, vector: u8, f: fn()) -> Result<(),()> {
        // required to drop the mutex guard before `if let`
        let contains = self.arr[vector as usize].read().clone();

        return if let None = contains {
            let mut inner = self.arr[vector as usize].write();
            *inner = Some(f);
            Ok(())
        } else { Err(()) }

    }


    /// unsets the given vector handler.
    pub unsafe fn unset(&self, vector: u8) {
        *self.arr[vector as usize].write() = None
    }

    /// Returns a reference to to the given vector handler.
    pub fn get(&self, vector: u8) -> &spin::RwLock<Option<fn()>> {
        &self.arr[vector as usize]
    }
}

