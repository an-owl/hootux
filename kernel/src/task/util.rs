mod sleep;

pub use crate::time::Duration;
use core::sync::atomic;
use core::task::{Context, Poll};
pub use sleep::Timer;

/// Sleeps for the number of specified milliseconds
pub async fn sleep(msec: u64) {
    Timer::new(Duration::millis(msec)).await
}

pub(crate) fn check_slp() {
    sleep::SLEEP_QUEUE.wakeup()
}

#[doc(hidden)]
pub mod suspend {
    use core::pin::Pin;
    use core::task::{Context, Poll};

    #[doc(hidden)]
    pub struct Suspend {
        suspend: bool,
    }

    impl Suspend {
        pub fn new() -> Self {
            Self { suspend: true }
        }
    }

    impl core::future::Future for Suspend {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            if self.suspend {
                Poll::Ready(())
            } else {
                self.suspend = false;
                cx.waker().clone().wake();
                Poll::Pending
            }
        }
    }

    /// Suspends the current task, to the back of the FIFO queue.
    /// This can be used to wait for locks to be freed by another task without using [sleep].
    #[macro_export]
    macro_rules! suspend {
        () => {
            $crate::task::util::suspend::Suspend::new().await;
        };
    }
}

/// A waker which does not wake.
pub struct DummyWaker;
impl alloc::task::Wake for DummyWaker {
    fn wake(self: alloc::sync::Arc<Self>) {}
}

#[macro_export]
macro_rules! block_on {
    ($task:expr_2021) => {{
        let mut future = $task;
        loop {
            match core::future::Future::poll(
                ::core::pin::Pin::new(&mut future),
                &mut ::core::task::Context::from_waker(&::core::task::Waker::from(
                    ::alloc::sync::Arc::new($crate::task::util::DummyWaker),
                )),
            ) {
                ::core::task::Poll::Pending => {
                    core::hint::spin_loop(); // busy-loop, but whatever
                    continue;
                }
                ::core::task::Poll::Ready(t) => break t,
            };
        }
    }};
}

/// Allows polling a future once. This may be to allow configuring the waker, or to initialise
/// a command to hardware without waiting on completion.
#[macro_export]
macro_rules! poll_once {
    ($fut:expr) => {
        ::core::future::Future::poll(
            $fut,
            &mut ::core::task::Context::from_waker(&core::task::Waker::noop()),
        )
    };
}

pub use block_on;

#[derive(Default)]
pub struct WorkerWaiter {
    waker: futures_util::task::AtomicWaker,
    run: atomic::AtomicBool,
}

impl WorkerWaiter {
    pub const fn new() -> Self {
        Self {
            waker: futures_util::task::AtomicWaker::new(),
            run: atomic::AtomicBool::new(false),
        }
    }

    pub fn wake(&self) {
        self.run.store(true, atomic::Ordering::Release);
        self.waker.wake();
    }

    pub async fn wait(&self) {
        WorkerWaiterFuture {
            worker_waiter: self,
        }
        .await;
    }
}

struct WorkerWaiterFuture<'a> {
    worker_waiter: &'a WorkerWaiter,
}

impl Future for WorkerWaiterFuture<'_> {
    type Output = ();
    fn poll(self: core::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.worker_waiter.run.compare_exchange(
            true,
            false,
            atomic::Ordering::Release,
            atomic::Ordering::Relaxed,
        ) {
            Ok(_) => Poll::Ready(()),
            Err(_) => {
                self.worker_waiter.waker.register(cx.waker());
                Poll::Pending
            }
        }
    }
}
