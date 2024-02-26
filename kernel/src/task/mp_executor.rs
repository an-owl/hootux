use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use core::future::Future;
use core::pin::Pin;
use core::task::{Poll, Waker};

const RUN_QUEUE_SIZE: usize = 128;

static GLOBAL_TASK_CACHE: TaskCache = TaskCache::new();
type TaskableFuture = Pin<Box<dyn Future<Output = super::TaskResult> + Send + 'static>>;

#[derive()]
struct TaskWaker {
    task: Weak<Task>,
    id: super::TaskId,
}

impl alloc::task::Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        if let Some(task) = self.task.upgrade() {
            super::SYS_EXECUTOR.read().get(&task.owner.load(atomic::Ordering::Relaxed).num()).unwrap()
                .run_queue
                .push(self.id)
                .expect("Run queue is full");
        } // return on else because if the upgrade fails then the task has been dropped and cannot be run anyway
    }
}


pub struct Task {
    id: super::TaskId,
    inner: crate::util::mutex::MentallyUnstableMutex<TaskableFuture>, // should not be accessed multiple times
    owner: atomic::Atomic<TaskOwner>,
    waker: spin::Mutex<Weak<TaskWaker>>,
}

#[derive(Copy, Clone, Debug)]
enum TaskOwner {
    Cpu(u32),
}

impl TaskOwner {
    fn num(&self) -> u32 {
        match self {
            TaskOwner::Cpu(n) => *n,
        }
    }
}

impl Task {
    pub fn new(fut: impl Future<Output = super::TaskResult> + Send + 'static) -> Self {
        Self {
            id: super::TaskId::new(),
            inner: crate::util::mutex::MentallyUnstableMutex::new(Box::pin(fut)),
            owner: atomic::Atomic::new(TaskOwner::Cpu(crate::who_am_i())),
            waker: spin::Mutex::new(Weak::new()),
        }
    }

    /// Returns a waker for `self`. If a Waker does not already exist one will be constructed.
    fn waker(self: &Arc<Self>) -> Waker {
        let mut l = self.waker.lock();
        if let Some(waker) = l.upgrade() {
            Waker::from(waker)
        } else {
            let b = Arc::new(TaskWaker {
                task: Arc::downgrade(self),
                id: self.id,
            });
            *l = Arc::downgrade(&b);
            Waker::from(b)
        }
    }

    /// Convenience function for polling the future within `self`.
    fn poll(&self, cx: &mut core::task::Context) -> Poll<super::TaskResult> {
        let ref mut t = *self.inner.lock();
        t.as_mut().poll(cx)
    }
}

pub(super)struct TaskCache {
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

pub(super) struct LocalExec {
    i: u32,
    /// This is just a semaphore that other CPUs can set to indicate
    /// this CPU should invalidate its task cache.
    invalidate: core::sync::atomic::AtomicBool,
    run_queue: crossbeam_queue::ArrayQueue<super::TaskId>, // todo swap with a linked list, each node should point to a waker and never directly allocate/free memory
    cache: crate::util::mutex::ReentrantMutex<LocalExecCache>
}

struct LocalExecCache {
    waker_cache: alloc::collections::BTreeMap<super::TaskId, Waker>,
    local_cache: alloc::collections::BTreeMap<super::TaskId, Arc<Task>>,
}

impl LocalExec {
    pub(super) fn new() -> Self {
        Self {
            i: crate::who_am_i(),
            invalidate: false.into(),
            run_queue: crossbeam_queue::ArrayQueue::new(RUN_QUEUE_SIZE),
            cache: crate::util::mutex::ReentrantMutex::new(LocalExecCache {
                waker_cache: alloc::collections::BTreeMap::new(),
                local_cache: alloc::collections::BTreeMap::new(),
            })
        }
    }

    fn run_ready(&self) {

        // when another CPU steals a task the cache should be invalidated
        if let Ok(_) = self.invalidate.compare_exchange_weak(true,false,atomic::Ordering::Relaxed,atomic::Ordering::Relaxed) {
            let mut l = self.cache.lock();
            l.local_cache.clear();
            l.waker_cache.clear();
        }

        while let Some(id) = self.fetch_ready() {
            let task = if let Some(task) = self.fetch_task(id) {
                task
            } else {
                continue
            };

            if task.owner.load(atomic::Ordering::Relaxed).num() != self.i {
                // Some tasks may need to be woken a certain number of times
                // This forwards wakeups to the new CPU instead of running here.
                task.waker().wake();
                continue;
            }

            let waker = self
                .cache
                .try_lock().unwrap() // shouldn't panic
                .waker_cache
                .entry(id)
                .or_insert_with(|| task.waker())
                .clone();

            match task.poll(&mut core::task::Context::from_waker(&waker)) {
                // todo impl Display for task and display more info here
                Poll::Ready(super::TaskResult::Error) => log::error!("{id:?} Exited with error"), // task should log error info
                Poll::Ready(super::TaskResult::Panicked) => {
                    log::error!("{id:?} Panicked and was caught successfully") // todo implement Display for Task
                }
                _ => {} // ops normal
            }

            GLOBAL_TASK_CACHE.drop(id);
        }
    }

    fn fetch_task(&self, id: super::TaskId) -> Option<Arc<Task>> {
        let mut l = self.cache.lock();
        if let Some(w) = l.local_cache.get(&id) {
            return Some(w.clone());
        } else if let Some(w) = GLOBAL_TASK_CACHE.fetch(id) {
            l.local_cache.insert(id, w.clone());
            return Some(w);
        }
        None
    }

    /// Fetches a [super::TaskId] from the local executors run queue,
    /// if nothing is ready then a task will be stolen
    fn fetch_ready(&self) -> Option<super::TaskId> {
        match self.run_queue.pop() {
            None => Self::steal(),
            Some(id) => return Some(id),
        }
    }

    /// Steals a Task from another CPU dropping it from that Executors caches and returning a [super::TaskID]
    fn steal() -> Option<super::TaskId> {
        // iterates over all CPUs except for self
        for _ in 0u32..crate::mp::num_cpus().into() {
            static ARBITRATION: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
            let target_id = ARBITRATION.fetch_add(1, atomic::Ordering::Relaxed) % crate::mp::num_cpus();
            // don't steal from self
            if target_id == crate::who_am_i() {
                continue;
            }

            let b = super::SYS_EXECUTOR.read();
            if let Some(target) = b.get(&target_id) {
                if let Some(id) = target.run_queue.pop() {

                    target.invalidate.store(true,atomic::Ordering::Relaxed);
                    GLOBAL_TASK_CACHE
                        .fetch(id)
                        .unwrap()
                        .owner
                        .store(TaskOwner::Cpu(crate::who_am_i()), atomic::Ordering::Relaxed);
                    return Some(id);
                }
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

    pub(super) fn run(&self) -> ! {
        loop {
            self.run_ready();
            self.idle()
        }
    }

    pub fn spawn(&self, task: Task) {
        let task = Arc::new(task);
        let id = task.id;
        GLOBAL_TASK_CACHE.insert(task.clone());
        self.cache.lock().local_cache.insert(id,task);
        self.run_queue.push(id).expect("Run queue is full");
    }
}