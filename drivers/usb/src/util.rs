use core::pin::Pin;
use core::sync::atomic;
use core::task::{Context, Poll};

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
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
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
