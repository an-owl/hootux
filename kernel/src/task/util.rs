mod sleep;

pub use crate::time::Duration;
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

pub use block_on;
