use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, Ordering};
use core::task::{Context, Poll};
use core::{future::Future, pin::Pin};

pub mod executor;
pub mod int_message_queue;
pub mod keyboard;
pub mod mp_executor;
pub mod simple_executor;
pub mod util;

static SYS_EXECUTOR: spin::RwLock<
    alloc::collections::BTreeMap<crate::mp::CpuIndex, mp_executor::LocalExec>,
> = spin::RwLock::new(alloc::collections::BTreeMap::new());

/// InterruptQueue is the type to pass interrupt message to drivers. The exact type used is
/// unimportant however it is wrapped in an [alloc::sync::Arc], so it may be shared between multiple places.
pub type InterruptQueue = alloc::sync::Arc<crossbeam_queue::ArrayQueue<InterruptMessage>>;

fn new_int_queue(len: usize) -> InterruptQueue {
    alloc::sync::Arc::new(crossbeam_queue::ArrayQueue::new(len))
}

#[derive(Copy, Clone, Debug)]
pub struct InterruptMessage(pub u64);

pub struct Task {
    id: TaskId,
    future: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl Task {
    pub fn new(future: impl Future<Output = ()> + Send + 'static) -> Self {
        Self {
            id: TaskId::new(),
            future: Box::pin(future),
        }
    }
    fn poll(&mut self, context: &mut Context) -> Poll<()> {
        self.future.as_mut().poll(context)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy)]
/// Return type returned by all top-level tasks.
/// The variant indicates what state the task exited in and may cause the kernel to take action.
#[repr(C)]
pub enum TaskResult {
    /// Task completed and exited normally or was shut down by the kernel
    ExitedNormally,
    /// Task stopped because it was signalled externally to do so.
    /// This may indicate that task encountered an error, or the device was removed, and should log info about why the task was stopped.
    StoppedExternally,
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

pub fn run_task(fut: Pin<Box<dyn Future<Output = TaskResult> + Send>>) {
    let t = mp_executor::Task::new(fut);

    let b = SYS_EXECUTOR.upgradeable_read();
    if let Some(e) = b.get(&crate::who_am_i()) {
        // You started an AP without reworking the executor for MP if this panics
        e.spawn(t);
    } else {
        let mut w = b.upgrade();
        w.insert(crate::who_am_i(), mp_executor::LocalExec::new());
        w.get(&crate::who_am_i()).unwrap().spawn(t);
    }
}

pub fn run_exec() -> ! {
    SYS_EXECUTOR.read().get(&crate::who_am_i()).unwrap().run()
}

/// Initializes the executor for the current CPU.
///
/// # Panics
///
/// This fn will panic if it has already been called on this CPU.
#[track_caller]
pub(crate) fn ap_setup_exec() {
    let rc = SYS_EXECUTOR
        .write()
        .insert(crate::who_am_i(), mp_executor::LocalExec::new());
    if let Some(_) = rc {
        panic!("Attempted to initialize CPU executor when it is already initialized")
    }
}
