use alloc::vec::Vec;
use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use core::future::Future;
use core::pin::Pin;
use core::task::{Poll, Waker};

const RUN_QUEUE_SIZE: usize = 128;

static GLOBAL_TASK_CACHE: TaskCache = TaskCache::new();
static GLOBAL_EXECUTORS: spin::RwLock<Vec<spin::RwLock<LocalExec>>> = spin::RwLock::new(Vec::new());

type TaskableFuture = Pin<Box<dyn Future<Output = TaskResult> + Send + 'static>>;

#[derive()]
struct TaskWaker {
    task: Weak<Task>,
    id: super::TaskId,
}

impl alloc::task::Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        if let Some(task) = self.task.upgrade() {
            GLOBAL_EXECUTORS.read()[task.owner.load(atomic::Ordering::Relaxed).num() as usize]
                .read()
                .run_queue
                .push(self.id)
                .expect("Run queue is full");
        } // return on else because if the upgrade fails then the task has been dropped and cannot be run anyway
    }
}

#[derive(Debug, Clone, Copy)]
/// Return type returned by all top-level tasks.
/// The variant indicates what state the task exited in and may cause the kernel to take action.
#[repr(C)]
enum TaskResult {
    /// Task completed and exited normally or was shut down by the kernel
    ExitedNormally,
    /// Task encountered an error and was unable to continue.
    Error,
    /// The task panicked and was able to catch the panic and guarantee that the system stability not impacted.
    Panicked,
    // Why is there no uncaught panic here? Because I cant return that.
}

impl From<()> for TaskResult {
    fn from(_: ()) -> Self {
        TaskResult::ExitedNormally
    }
}

struct Task {
    id: super::TaskId,
    inner: crate::util::mutex::MentallyUnstableMutex<TaskableFuture>, // should not be accessed multiple times
    owner: atomic::Atomic<TaskOwner>,
    waker: spin::Mutex<Weak<TaskWaker>>,
}

#[derive(Copy, Clone, Debug)]
enum TaskOwner {
    Orphan,
    Cpu(u32),
}

impl TaskOwner {
    fn num(&self) -> u32 {
        match self {
            TaskOwner::Orphan => 0,
            TaskOwner::Cpu(n) => *n,
        }
    }
}

impl Task {
    fn new(fut: impl Future<Output = TaskResult> + Send + 'static) -> Self {
        Self {
            id: super::TaskId::new(),
            inner: crate::util::mutex::MentallyUnstableMutex::new(Box::pin(fut)),
            owner: atomic::Atomic::new(TaskOwner::Orphan),
            waker: spin::Mutex::new(Weak::new()),
        }
    }

    /// Returns a waker for `self`. If a Waker does not already exist one will be constructed.
    fn waker(self: &Arc<Self>) -> Waker {
        if let Some(waker) = self.waker.lock().upgrade() {
            Waker::from(waker)
        } else {
            let b = Arc::new(TaskWaker {
                task: Arc::downgrade(self),
                id: self.id,
            });
            *self.waker.lock() = Arc::downgrade(&b);
            Waker::from(b)
        }
    }

    /// Convenience function for polling the future within `self`.
    fn poll(&self, cx: &mut core::task::Context) -> Poll<TaskResult> {
        let ref mut t = *self.inner.lock();
        t.as_mut().poll(cx)
    }
}

struct TaskCache {
    cache: spin::RwLock<alloc::collections::BTreeMap<super::TaskId, Arc<Task>>>,
}

impl TaskCache {
    const fn new() -> Self {
        Self {
            cache: spin::RwLock::new(alloc::collections::BTreeMap::new()),
        }
    }

    fn fetch(&self, id: super::TaskId) -> Option<Arc<Task>> {
        Some(self.cache.read().get(&id)?.clone())
    }

    #[track_caller]
    fn insert(&self, task: Arc<Task>) {
        self.cache
            .write()
            .insert(task.id, task)
            .expect("Tried to insert a task to the global task cache twice");
    }

    /// Drops the task with `id`
    fn drop(&self, id: super::TaskId) {
        let _ = self.cache.write().remove(&id);
    }
}

struct LocalExec {
    /// How long this executor has spent running.
    /// This will be compared with all other local executors to determine if work needs to be moved to other CPUs.  
    load_time: core::sync::atomic::AtomicU64,
    start_time: core::sync::atomic::AtomicU64,
    stop_time: core::sync::atomic::AtomicU64,
    active: spin::Mutex<Option<super::TaskId>>,

    i: u32,
    local_cache: alloc::collections::BTreeMap<super::TaskId, Arc<Task>>,
    run_queue: crossbeam_queue::ArrayQueue<super::TaskId>, // todo swap with a linked list, each node should point to a waker and never directly allocate/free memory
    waker_cache: alloc::collections::BTreeMap<super::TaskId, Waker>,
}

impl LocalExec {
    fn new() -> Self {
        Self {
            load_time: core::sync::atomic::AtomicU64::new(0),
            start_time: core::sync::atomic::AtomicU64::new(0),
            stop_time: core::sync::atomic::AtomicU64::new(0),

            i: crate::who_am_i(),
            active: spin::Mutex::new(None),
            local_cache: alloc::collections::BTreeMap::new(),
            run_queue: crossbeam_queue::ArrayQueue::new(RUN_QUEUE_SIZE),
            waker_cache: alloc::collections::BTreeMap::new(),
        }
    }

    fn run_ready(&mut self) {
        let start = crate::time::get_sys_time();
        self.start_time.store(start, atomic::Ordering::SeqCst);

        while let Some(id) = self.run_queue.pop() {
            let task = self.fetch_task(id).expect(&alloc::format!(
                "{:?} Was woken but has no associated task",
                id
            ));

            if task.owner.load(atomic::Ordering::Relaxed).num() != self.i {
                // Some tasks may need to be woken a certain number of times
                // This forwards wakeups to the new CPU instead of running here.
                task.waker().wake();
                continue;
            }

            let waker = self
                .waker_cache
                .entry(id)
                .or_insert_with(|| task.waker())
                .clone();
            *self.active.lock() = Some(id);

            match task.poll(&mut core::task::Context::from_waker(&waker)) {
                // todo impl Display for task and display more info here
                Poll::Ready(TaskResult::Error) => log::error!("{id:?} Exited with error"), // task should log error info
                Poll::Ready(TaskResult::Panicked) => {
                    log::error!("{id:?} Panicked and was caught successfully") // todo implement Display for Task
                }
                _ => {} // ops normal
            }

            *self.active.lock() = None;
            GLOBAL_TASK_CACHE.drop(id);
        }

        let stop = crate::time::get_sys_time();
        self.stop_time.store(stop, atomic::Ordering::Release);
        self.load_time
            .fetch_add(stop - start, atomic::Ordering::Release);
    }

    fn fetch_task(&mut self, id: super::TaskId) -> Option<Arc<Task>> {
        if let Some(w) = self.local_cache.get(&id) {
            return Some(w.clone());
        } else if let Some(w) = GLOBAL_TASK_CACHE.fetch(id) {
            self.local_cache.insert(id, w.clone());
            return Some(w);
        }
        None
    }

    /// Steals a Task from another CPU dropping it from that Executors caches and returning a [super::TaskID]
    fn steal() -> Option<super::TaskId> {
        // fixme 0..num_cpus()
        // iterates over all CPUs except for self
        for _ in 0..todo!() {
            static ARBITRATION: core::sync::atomic::AtomicU32 = const { 0.into() };
            let target_id = ARBITRATION.fetch_add(1, atomic::Ordering::Relaxed) as usize; // mod num_cpus
            if target_id == crate::who_am_i() {
                continue;
            }
            let target = GLOBAL_EXECUTORS.read()[target_id].upgradeable_read();
            if let Some(id) = target.run_queue.pop() {
                let target = target.upgrade();
                GLOBAL_TASK_CACHE
                    .fetch(id)
                    .unwrap()
                    .owner
                    .store(TaskOwner::Cpu(crate::who_am_i()), atomic::Ordering::Relaxed);
                target.local_cache.remove(&id);
                target.waker_cache.remove(&id);
                return Some(id);
            }
        }
        None
    }

    fn idle(&self) {
        use x86_64::instructions::interrupts;

        interrupts::disable();
        if self.run_queue.is_empty() {
            interrupts::enable_and_hlt();
        } else {
            interrupts::enable();
        }
    }
}
