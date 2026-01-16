use crate::mp::bitmap::CpuBMap;
use crate::task::{TaskId, TaskResult};
use crate::util::PhantomUnsend;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use futures_util::Stream;

const WORK_QUEUE_SIZE: usize = 128;

#[thread_local]
static CONTEXT: ContextPointer = ContextPointer::empty();

static GLOBAL_EXECUTOR: GlobalExecutorState = GlobalExecutorState::new();

struct ContextPointer {
    context: core::sync::atomic::AtomicPtr<TaskContext>,
}

impl ContextPointer {
    const fn empty() -> Self {
        Self {
            context: core::sync::atomic::AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    /// Fetches a reference to the current task context is its present.
    /// The context is guaranteed to not be open in another thread.
    fn get_context(&self) -> Option<*mut TaskContext> {
        let ptr = self.context.load(atomic::Ordering::Relaxed);
        if ptr.is_null() { None } else { Some(ptr) }
    }

    /// Sets the current context. Returns a guard that will clear the context when dropped.
    fn set_context<'a>(&self, context: &'a mut TaskContext) -> ContextPointerGuard<'a> {
        if self
            .context
            .swap(context, atomic::Ordering::Acquire)
            .is_null()
        {
            ContextPointerGuard { _p: PhantomData }
        } else {
            panic!(
                "Attempted to run multiple tasks on CPU {}",
                crate::who_am_i()
            );
        }
    }
}

pub struct ContextContainer {
    pub context: *mut TaskContext,
    _phantom_unsend: PhantomUnsend,
}

impl ContextContainer {
    pub fn get() -> Option<Self> {
        Some(Self {
            context: CONTEXT.get_context()?,
            _phantom_unsend: Default::default(),
        })
    }

    /// This fetches the current tasks context.
    /// This cannot be held across `await`'s.
    ///
    /// # Safety
    ///
    /// See [core::ptr]. The context is guaranteed to not be accessed by another thread.
    pub unsafe fn get_context(&self) -> &mut TaskContext {
        unsafe { &mut *self.context }
    }
}

struct ContextPointerGuard<'a> {
    _p: PhantomData<&'a ()>,
}

impl Drop for ContextPointerGuard<'_> {
    fn drop(&mut self) {
        CONTEXT
            .context
            .store(core::ptr::null_mut(), atomic::Ordering::Release);
    }
}

/// This contains kernel executor state that is local to this CPU.
/// Tasks are split into 2 queues, one which can be stolen from and one which cant.
/// A monotonic number is given to each job in the work queue, this is used to maintain the FIFO nature
/// of the kernel executor.
///
/// FIFO for kernel work is expected but is not a strict requirement.
/// The kernel should prevent idling so its expected that FIFO will violated when
///
/// - The executor cannot lock a resource required to run the next job.
/// - Work has been stolen from another CPU.
/// - "this" CPU cannot run the task for whatever reason.
pub(crate) struct LocalExecutor {
    /// This arbiter indicates which queue should be preferred.
    /// When `true` the scheduler should use the local queue.
    arbiter_local: core::sync::atomic::AtomicBool,
    local_queue: Arc<crossbeam_queue::ArrayQueue<TaskId>>,
    stealable_queue: Arc<crossbeam_queue::ArrayQueue<TaskId>>,

    /// Contains only local tasks.
    ///
    /// I don't think this actually needs a mutex, because this field is only accessed by the CPU
    /// that *owns* this executor.
    local_task_cache: spin::Mutex<alloc::collections::BTreeMap<TaskId, Arc<spin::Mutex<Task>>>>,
}

impl LocalExecutor {
    pub(crate) fn new() -> LocalExecutor {
        Self {
            arbiter_local: core::sync::atomic::AtomicBool::new(false),

            local_queue: Arc::new(crossbeam_queue::ArrayQueue::new(WORK_QUEUE_SIZE)),
            stealable_queue: Arc::new(crossbeam_queue::ArrayQueue::new(WORK_QUEUE_SIZE)),
            local_task_cache: spin::Mutex::new(alloc::collections::BTreeMap::new()),
        }
    }

    pub(crate) fn run(&self) -> ! {
        loop {
            self.run_kernel();
            self.run_stolen();
            // Run user
            self.run_idle_work();
            self.idle()
        }
    }

    fn run_kernel(&self) {
        loop {
            let Some(task) = self.fetch_kernel_task() else {
                return;
            };
            self.run_task(task);
        }
    }

    fn fetch_kernel_task(&self) -> Option<Arc<spin::Mutex<Task>>> {
        let Some((tid, local)) = (if self.local_queue.len() >= self.stealable_queue.len() {
            self.arbiter_local.store(false, atomic::Ordering::Relaxed);
            self.local_queue
                .pop()
                .map(|e| (e, true))
                .or_else(|| self.stealable_queue.pop().map(|e| (e, false)))
        } else if self.arbiter_local.load(atomic::Ordering::Relaxed) {
            self.arbiter_local.store(false, atomic::Ordering::Relaxed);
            self.local_queue
                .pop()
                .map(|e| (e, true))
                .or_else(|| self.stealable_queue.pop().map(|e| (e, false)))
        } else {
            self.arbiter_local.store(true, atomic::Ordering::Relaxed);
            self.stealable_queue
                .pop()
                .map(|e| (e, false))
                .or_else(|| self.local_queue.pop().map(|e| (e, true)))
        }) else {
            return None; // Work exhausted
        };

        let task = if local {
            self.fetch_from_cache(tid)
        } else {
            GLOBAL_EXECUTOR.global_task_list.read().get(&tid).cloned()?
        };

        Some(task)
    }

    /// Fetches a local task. If it's not in the local cache then the local cache will be filled.
    fn fetch_from_cache(&self, tid: TaskId) -> Arc<spin::Mutex<Task>> {
        let mut l = self.local_task_cache.lock();
        if let Some(task) = l.get(&tid) {
            task.clone()
        } else {
            let task = GLOBAL_EXECUTOR
                .global_task_list
                .read()
                .get(&tid)
                .expect("No task found")
                .clone();
            l.insert(tid, task.clone());
            task
        }
    }

    fn run_task(&self, task: Arc<spin::Mutex<Task>>) {
        let Some(mut task) = task.try_lock() else {
            return;
        };
        if !task.is_valid() {
            let id = task.context.id;
            drop(task); // prevents race where other scheduler may fail to lock, assuming that another CPU is running the task.
            GLOBAL_EXECUTOR.advertised_work.push(id);
            return;
        }
        let waker = match task.waker {
            Some(ref waker) => waker.clone(),
            None => {
                let waker = KernelWaker::new(&task.context);
                task.waker = Some(waker.clone());
                waker
            }
        };

        match task.poll(Context::from_waker(&waker.into())) {
            Poll::Ready(e @ TaskResult::ExitedNormally | e @ TaskResult::StoppedExternally) => {
                log::trace!("Task {} exited with {e:?}", task.context);
                let _ = GLOBAL_EXECUTOR
                    .global_task_list
                    .write()
                    .remove(&task.context.id);
            }
            Poll::Ready(TaskResult::Error) => {
                log::error!("Task {} exited with error", task.context);
                let _ = GLOBAL_EXECUTOR
                    .global_task_list
                    .write()
                    .remove(&task.context.id);
            }
            Poll::Ready(TaskResult::Panicked) => {
                log::error!("Task {} panicked and was caught", task.context);
                let _ = GLOBAL_EXECUTOR
                    .global_task_list
                    .write()
                    .remove(&task.context.id);
            }
            Poll::Pending => {
                if task.context.reload_waker.load(atomic::Ordering::Relaxed) {
                    task.waker.as_ref().unwrap().reload(&task.context);
                }
            }
        }
    }

    fn run_idle_work(&self) {
        while let Some(t) = GLOBAL_EXECUTOR.idle_jobs.search() {
            t.poll();
        }
    }

    fn idle(&self) {
        x86_64::instructions::interrupts::disable();
        if self.local_queue.is_empty() && self.stealable_queue.is_empty() {
            x86_64::instructions::interrupts::enable_and_hlt()
        } else {
            x86_64::instructions::interrupts::enable();
        }
    }

    pub(crate) fn spawn(&self, task: Task) {
        let mut task_list = GLOBAL_EXECUTOR.global_task_list.write();
        let id = task.context.id;
        let a = Arc::new(spin::Mutex::new(task));
        task_list.insert(id, a.clone());
        self.stealable_queue.push(id).expect("stealable queue full");
    }

    fn run_stolen(&self) {
        while let Some(task) = self.steal_work() {
            self.run_task(task)
        }
    }

    fn steal_work(&self) -> Option<Arc<spin::Mutex<Task>>> {
        for _ in 0..GLOBAL_EXECUTOR.advertised_work.len() {
            let Some(taskid) = GLOBAL_EXECUTOR.advertised_work.pop() else {
                break;
            };
            let Some(task) = GLOBAL_EXECUTOR
                .global_task_list
                .read()
                .get(&taskid)
                .cloned()
            else {
                continue;
            };
            let Some(locked) = task.try_lock() else {
                continue;
            };
            if locked.is_valid() {
                drop(locked);
                return Some(task);
            }
        }

        {
            static ARBITER: core::sync::atomic::AtomicUsize =
                core::sync::atomic::AtomicUsize::new(0);
            let t = ARBITER.fetch_add(1, atomic::Ordering::Relaxed) as u32 % crate::mp::num_cpus();
            let rl = super::SYS_EXECUTOR.read();
            let exec = rl.get(&t).unwrap();
            exec.steal()
        }
    }

    fn steal(&self) -> Option<Arc<spin::Mutex<Task>>> {
        for _ in 0..self.stealable_queue.len() {
            let tid = self.stealable_queue.pop()?;
            let Some(task) = GLOBAL_EXECUTOR.global_task_list.read().get(&tid).cloned() else {
                return None;
            };
            let Some(locked) = task.try_lock() else {
                self.stealable_queue
                    .push(tid)
                    .expect("stealable queue full");
                continue;
            };
            if locked.is_valid() {
                drop(locked);
                return Some(task);
            } else {
                self.stealable_queue
                    .push(tid)
                    .expect("stealable queue full");
            };
        }
        None
    }
}

struct GlobalExecutorState {
    cpus_working: core::sync::atomic::AtomicU32,

    /// This queue contains kernel work that could not be run the CPU that received the work.
    /// All work in the advertised queue must be completed before stealing other work.
    advertised_work: crossbeam_queue::SegQueue<TaskId>,
    /// See [IdleTask]
    idle_jobs: IdleWorkList,

    /// Contains all kernel tasks (except idle ones).
    global_task_list: spin::RwLock<alloc::collections::BTreeMap<TaskId, Arc<spin::Mutex<Task>>>>,
}

impl GlobalExecutorState {
    const fn new() -> GlobalExecutorState {
        Self {
            cpus_working: core::sync::atomic::AtomicU32::new(0),
            advertised_work: crossbeam_queue::SegQueue::new(),
            idle_jobs: IdleWorkList::new(),
            global_task_list: spin::RwLock::new(alloc::collections::BTreeMap::new()),
        }
    }
}

struct IdleWorkList {
    arbiter: core::sync::atomic::AtomicUsize,
    idle_jobs: spin::RwLock<Vec<Arc<IdleTask>>>,
}

impl IdleWorkList {
    const fn new() -> Self {
        Self {
            arbiter: core::sync::atomic::AtomicUsize::new(0),
            idle_jobs: spin::RwLock::new(Vec::new()),
        }
    }

    fn push(&self, task: IdleTask) {
        self.idle_jobs.write().push(Arc::new(task));
    }

    fn search(&self) -> Option<Arc<IdleTask>> {
        let l = self.idle_jobs.read();
        let start = self.arbiter.load(atomic::Ordering::Relaxed);
        let cpus_working = GLOBAL_EXECUTOR.cpus_working.load(atomic::Ordering::Relaxed);
        for (i, task) in l[start..]
            .iter()
            .enumerate()
            .chain(l[..start].iter().enumerate())
            .filter(|(_, t)| t.get_standdown_time().is_future())
        {
            if task.active_minimum <= cpus_working {
                let next_arbiter = if (start + i + 1) >= l.len() {
                    start + i + 1
                } else {
                    0
                };

                // This is subject to race conditions, but it doesn't really matter.
                // The purpose of the arbiter is just to stop the same idle task being run every time, blocking the rest.
                self.arbiter.store(next_arbiter, atomic::Ordering::Relaxed);
                return Some(task.clone());
            }

            // Search wrapped around to `start` so we dont update the arbiter. But from here the actual value doesnt matter.
            // todo: Should arbiter be incremented. Just to spice things up a little.
        }

        None
    }
}

pub struct Task {
    context: TaskContext,
    future: Pin<Box<dyn Future<Output = TaskResult> + Send>>,
    waker: Option<Arc<KernelWaker>>,
}

pub struct TaskContext {
    id: TaskId,
    /// CPUs set to `true` may not run this task.
    affinity: CpuBMap,
    /// Indicates the parent process of `self`.
    /// This will be assigned automatically when the task is started. If set to `None` then this
    /// indicates that this was not started within a task context.
    parent: Option<TaskId>,
    /// Thread name. This can be changed by the task at runtime.
    /// By default, this will be inherited from the parent process.
    pub name: String,
    /// If the task is associated with a device it should advertise the device ID here.
    pub device: Option<crate::fs::vfs::DevID>,
    /// Indicates whether the task has exited.
    exited: bool,
    /// Waker should be dropped on exit. Causing it to be rebuilt on next entry.
    reload_waker: core::sync::atomic::AtomicBool,
}

impl Task {
    /// Creates a new task from the current context.
    /// The CPU affinity, and name will be inherited from the parent.
    /// The device will not be inherited.
    ///
    /// # Panics
    ///
    /// This fn will panic if this is not called within an async context.
    pub fn new_from_context(
        name: Option<String>,
        future: Pin<Box<dyn Future<Output = TaskResult> + Send + 'static>>,
    ) -> Self {
        let Some(context) = CONTEXT.get_context() else {
            panic!(
                "Called `{}::new_from_context()` outside of async context.",
                core::any::type_name::<Self>()
            )
        };
        let cx = unsafe { &*context };
        let tc = TaskContext {
            id: TaskId::new(),
            affinity: CpuBMap::new(),
            parent: Some(cx.id),
            name: name.unwrap_or(cx.name.clone()),
            device: None,
            exited: false,
            reload_waker: core::sync::atomic::AtomicBool::new(false),
        };
        Self {
            context: tc,
            future,
            waker: None,
        }
    }

    /// Constructs a new task without inheriting the current context.
    pub fn new(future: Pin<Box<dyn Future<Output = TaskResult> + Send + 'static>>) -> Self {
        Self {
            context: TaskContext {
                id: TaskId::new(),
                affinity: CpuBMap::new(),
                parent: None,
                name: String::new(),
                device: None,
                exited: false,
                reload_waker: core::sync::atomic::AtomicBool::new(false),
            },
            future,
            waker: None,
        }
    }

    pub fn run(self) {
        let id = self.context.id;
        let this = Arc::new(spin::Mutex::new(self));
        GLOBAL_EXECUTOR
            .global_task_list
            .write()
            .insert(id, this.clone());
        super::SYS_EXECUTOR
            .read()
            .get(&crate::who_am_i())
            .unwrap()
            .stealable_queue
            .push(id)
            .expect_err("stealable queue full");
    }

    /// Determines whether this was called within an async context and calls either
    /// [Self::new] or [Self::new_from_context].
    /// This fn should only be preferred for internal use.
    pub fn new_maybe_context(
        future: Pin<Box<dyn Future<Output = TaskResult> + Send + 'static>>,
    ) -> Self {
        if ContextContainer::get().is_some() {
            Self::new_from_context(None, future)
        } else {
            Self::new(future)
        }
    }

    fn poll(&mut self, mut cx: Context<'_>) -> Poll<TaskResult> {
        if self.context.exited {
            log::error!("Attempted to run task {} which has exited", self.context);
            Poll::Ready(TaskResult::Error)
        } else {
            let _task_context = CONTEXT.set_context(&mut self.context);
            let rc = self.future.as_mut().poll(&mut cx);
            drop(_task_context);
            if let Poll::Ready(_) = rc {
                self.context.exited = true;
            };

            rc
        }
    }

    /// Determines if the caller can run this task.
    fn is_valid(&self) -> bool {
        !self.context.affinity.get(crate::who_am_i())
    }
}

impl TaskContext {
    pub fn parent(&self) -> Option<TaskId> {
        self.parent
    }

    pub fn id(&self) -> u64 {
        self.id.0
    }

    pub fn set_affinity(&mut self, cpu: crate::mp::CpuIndex, enabled: bool) {
        let enabled = !enabled;
        if enabled {
            self.affinity.set_bit(cpu);
        } else {
            self.affinity.clear_bit(cpu);
        };
        self.reload_waker.store(true, atomic::Ordering::Relaxed);
    }
}

impl core::fmt::Display for TaskContext {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "[{}] {} {}",
            self.id.0,
            self.name,
            if let Some(dev) = self.device {
                alloc::format!("for {dev}")
            } else {
                String::new()
            }
        )
    }
}

/// Idle tasks are work that should only be run when the kernel has nothing else to do.
/// These tasks should perform a minimum amount of work before calling [crate::suspend].
/// If it has no more work then it returns a stand-down time, the task will not be run for the
/// returned duration.
/// Idle tasks should never wait for or use any IO.
pub struct IdleTask {
    // SAFETY: Context is only modified from within `future` requiring `future` to be locked anyway.
    // `context` will not be accessed while `future` is locked.
    context: spin::Mutex<TaskContext>,
    /// Indicates the maximum number of CPUs that may be working when this task is started.
    active_minimum: crate::mp::CpuCount,
    /// This task will not be run until this time.
    stand_down_until: atomic::Atomic<crate::time::AbsoluteTime>,

    future: spin::Mutex<Pin<Box<dyn Stream<Item = crate::time::Duration> + Send + Sync + 'static>>>,
}

impl IdleTask {
    /// Spawns a new idle task.
    pub fn spawn(
        &self,
        job: Pin<Box<dyn Stream<Item = crate::time::Duration> + Sync + Send + 'static>>,
        name: String,
        min_working_cpus: crate::mp::CpuCount,
    ) {
        let context = TaskContext {
            id: TaskId::new(),
            affinity: CpuBMap::new(),
            parent: None,
            name,
            device: None,
            exited: false,
            reload_waker: core::sync::atomic::AtomicBool::new(false),
        };

        let task = IdleTask {
            context: spin::Mutex::new(context),
            active_minimum: min_working_cpus,
            stand_down_until: atomic::Atomic::new(crate::time::AbsoluteTime::now()),
            future: spin::Mutex::new(job),
        };

        GLOBAL_EXECUTOR.idle_jobs.push(task);
    }

    fn poll(&self) {
        let mut cl = self.context.lock();
        let _g = CONTEXT.set_context(&mut *cl);
        if let Poll::Ready(Some(t)) = self
            .future
            .lock()
            .as_mut()
            .poll_next(&mut Context::from_waker(Waker::noop()))
        {
            self.stand_down_until
                .store(t.into(), atomic::Ordering::Relaxed);
        }
    }

    fn get_standdown_time(&self) -> crate::time::AbsoluteTime {
        self.stand_down_until.load(atomic::Ordering::Relaxed)
    }
}

// SAFETY:
unsafe impl Sync for IdleTask {}
unsafe impl Send for IdleTask {}

struct KernelWaker {
    id: TaskId,
    /// Indicates whether this task should be pushed to the local queue.
    local: core::sync::atomic::AtomicBool,
    /// Indicates whether `owner` is valid.
    is_owned: core::sync::atomic::AtomicBool,
    owner: core::sync::atomic::AtomicU32,
}

impl KernelWaker {
    fn new(context: &TaskContext) -> Arc<Self> {
        let this = Self {
            id: context.id,
            local: core::sync::atomic::AtomicBool::new(false),
            is_owned: core::sync::atomic::AtomicBool::new(false),
            owner: core::sync::atomic::AtomicU32::new(0),
        };
        this.reload(context);
        Arc::new(this)
    }

    fn reload(&self, context: &TaskContext) {
        let mut allowed_cpus = context
            .affinity
            .count()
            .map(|e| crate::mp::num_cpus() - e)
            .unwrap_or(crate::mp::num_cpus());
        if allowed_cpus == 0 {
            log::error!(
                "Task {} no affinity. Affinity will be ignored",
                context.id.0
            );
            allowed_cpus = crate::mp::num_cpus();
        }

        if allowed_cpus == 1 {
            let owner = context.affinity.find_first_zero().unwrap(); // Unwrap
            self.owner.store(owner as u32, atomic::Ordering::Release);
            self.is_owned.store(true, atomic::Ordering::Release);
            self.local.store(true, atomic::Ordering::Release);
        } else {
            self.local.store(false, atomic::Ordering::Release);
            self.is_owned.store(false, atomic::Ordering::Release);
        }

        context.reload_waker.store(false, atomic::Ordering::Relaxed);
    }
}

impl alloc::task::Wake for KernelWaker {
    fn wake(self: Arc<Self>) {
        let local = self.local.load(atomic::Ordering::Relaxed);
        let owner = self.owner.load(atomic::Ordering::Relaxed);

        // In the event that a task is woken *before* the executor starts this will prevent a deadlock.
        // It's fine to return early without properly waking the process because the task was woken
        // when the task was spawned.
        let Some(rl) = super::SYS_EXECUTOR.try_read() else {
            return;
        };
        let id = self.id.0;

        match self.is_owned.load(atomic::Ordering::Relaxed) {
            false => {
                let Some(exec) = rl.get(&crate::who_am_i()) else {
                    unreachable!()
                };
                if local {
                    exec.local_queue.push(self.id).expect("task queue full");
                } else {
                    exec.stealable_queue.push(self.id).expect("task queue full");
                }
            }
            true => {
                let Some(exec) = rl.get(&owner) else {
                    unreachable!()
                };
                if local {
                    exec.local_queue.push(self.id).expect("task queue full");
                } else {
                    exec.stealable_queue.push(self.id).expect("task queue full");
                }
            }
        }
    }
}
