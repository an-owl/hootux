pub(super) static SLEEP_QUEUE: crate::kernel_structures::Mutex<SleepQueue> =
    crate::kernel_structures::Mutex::new(SleepQueue {
        list: alloc::collections::VecDeque::new(),
        dirty: false,
    });

// todo register handler for this into timer interrupt
#[derive(Debug)]
pub(crate) struct SleepQueue {
    list: alloc::collections::VecDeque<Option<Timer>>,
    // is this even worth it
    dirty: bool,
}

impl crate::kernel_structures::Mutex<SleepQueue> {
    /// Registers a new timer into self.
    fn register(&self, timer: &Timer) {
        // Without interrupts to prevent the mutex being locked during a timer interrupt
        x86_64::instructions::interrupts::without_interrupts(|| {
            let t = timer.clone();
            t.inner.in_queue.store(true, atomic::Ordering::Relaxed);
            // Locates the insertion point regardless of weather or not something exists there
            let mut l = self.lock();
            let s = Some(t);
            let index = l.list.binary_search(&s).unwrap_or_else(|i| i);
            l.list.insert(index, s);
        })
    }

    /// Wakes all timers which can be woken
    pub(crate) fn wakeup(&self) {
        let ct = crate::time::get_sys_time();
        let mut l = self.lock();
        let mut dirty = false;

        for i in l.list.iter_mut() {
            if let Some(w) = i {
                if w.try_wake(ct) {
                    // taking prevents calls to the allocator
                    i.take();
                    dirty = true;
                } else {
                    break;
                }
            } // ignore None, these are already woken
        }
        l.dirty = dirty;
    }

    /// Attempts to clean the queue. The queue will not be cleaned if it is locked by another thread
    /// or if the dirty bit is not set.  
    fn clean(&self) {
        // dont clean if locked, i don't want that overhead
        if let Some(mut l) = self.try_lock() {
            if l.dirty {
                while let Some(None) = l.list.front() {
                    l.list.pop_front();
                }
                l.dirty = false;
            }
        }
    }
}

/// A timer implementing [core::future::Future] which will be ready after a set duration.
/// Polling is at the mercy of the hardware clock source. The resolution of wakeups is at the mercy
/// of the system clock events which themselves are at the mercy of the hardware clock source.
#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
pub struct Timer {
    inner: alloc::sync::Arc<SleepTimerInner>,
}

impl Timer {
    /// Creates a new instance of self which will be ready in `duration` nanoseconds
    pub fn new(duration: super::Duration) -> Self {
        let time = crate::time::get_sys_time();
        Self {
            inner: alloc::sync::Arc::new(SleepTimerInner {
                in_queue: false.into(),
                alarm: time + duration.get_nanos(),
                waker: Default::default(),
            }),
        }
    }
    /// Compares the given time to the inner alarm. If the given time is greater or equal to the
    /// alarm the future is woken. Returns weather the future was woken.
    fn try_wake(&self, time: u64) -> bool {
        if time < self.inner.alarm {
            false
        } else {
            self.inner.waker.wake();
            true
        }
    }
}

impl core::future::Future for Timer {
    type Output = ();

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        let t = crate::time::get_sys_time();
        if t < self.inner.alarm {
            if !self.inner.in_queue.load(atomic::Ordering::Relaxed) {
                self.inner.waker.register(cx.waker());
                SLEEP_QUEUE.register(&self);
            }

            core::task::Poll::Pending
        } else {
            SLEEP_QUEUE.clean();
            core::task::Poll::Ready(())
        }
    }
}

#[derive(Debug)]
struct SleepTimerInner {
    in_queue: core::sync::atomic::AtomicBool,
    alarm: u64,
    waker: futures_util::task::AtomicWaker,
}

impl Eq for SleepTimerInner {}

impl PartialEq<Self> for SleepTimerInner {
    fn eq(&self, other: &Self) -> bool {
        self.alarm.eq(&other.alarm)
    }
}

impl Ord for SleepTimerInner {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.alarm.cmp(&other.alarm)
    }
}

impl PartialOrd for SleepTimerInner {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.alarm.partial_cmp(&other.alarm)
    }
}
