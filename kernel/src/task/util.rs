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

mod suspend {
    use core::pin::Pin;
    use core::task::{Context, Poll};

    #[doc(hidden)]
    struct Suspend {
        suspend: bool,
    }

    impl Suspend {
        fn new() -> Self {
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
            suspend::Suspend::new().await;
        };
    }
}
