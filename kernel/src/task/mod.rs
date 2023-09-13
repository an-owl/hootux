use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, Ordering};
use core::task::{Context, Poll};
use core::{future::Future, pin::Pin};

pub mod executor;
pub mod int_message_queue;
pub mod keyboard;
pub mod simple_executor;
pub mod util;

// This uses a ReentrantMutex because it needs to be able to spawn tasks while it's running.
// In the future it can use an orphan list that is shared between other CPU's and pull tasks from it before it halts.
lazy_static::lazy_static! {
    static ref SYS_EXECUTOR: crate::kernel_structures::mutex::ReentrantMutex<executor::Executor> = crate::kernel_structures::mutex::ReentrantMutex::new(executor::Executor::new());
}

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
