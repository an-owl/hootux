mod sleep;

pub use crate::time::Duration;
use alloc::sync::Arc;
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

struct MessageFutureInner<T = ()> {
    waker: futures_util::task::AtomicWaker,
    completed: atomic::AtomicU8,
    data: core::cell::UnsafeCell<core::mem::MaybeUninit<T>>,
}

impl MessageFutureInner {
    const SEM_NOT_READY: u8 = 0;
    const SEM_READY: u8 = 1;
    const SEM_COMPLETED: u8 = 2;
}

pub struct MessageFuture<X: MessageDirection = Receiver, T = ()> {
    inner: Arc<MessageFutureInner<T>>,
    _phantom: core::marker::PhantomData<X>,
}

marker_maker::impl_marker! {
    pub trait MessageDirection {
        pub Sender,
        pub Receiver,
    }
}

pub fn new_message<T>() -> (MessageFuture<Sender, T>, MessageFuture<Receiver, T>) {
    let sender = MessageFuture {
        inner: Arc::new(MessageFutureInner {
            waker: futures_util::task::AtomicWaker::new(),
            completed: atomic::AtomicU8::new(MessageFutureInner::SEM_NOT_READY),
            data: core::cell::UnsafeCell::new(core::mem::MaybeUninit::uninit()),
        }),
        _phantom: core::marker::PhantomData,
    };
    let receiver = MessageFuture {
        inner: sender.inner.clone(),
        _phantom: core::marker::PhantomData,
    };
    (sender, receiver)
}

impl<T> MessageFuture<Sender, T> {
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn complete(self, message: T) {
        const ERR: &str = "Attempted to complete FsedMessage twice";
        if self.inner.completed.load(atomic::Ordering::Acquire) != MessageFutureInner::SEM_NOT_READY
        {
            panic!("{}", ERR)
        }

        // SAFETY: Semaphore indicates that we can send data.
        unsafe { &mut *self.inner.data.get() }.write(message);
        self.inner
            .completed
            .store(MessageFutureInner::SEM_READY, atomic::Ordering::Release);
        self.inner.waker.wake();
    }
}

impl<T> Future for MessageFuture<Receiver, T> {
    type Output = T;
    fn poll(self: core::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.inner.completed.load(atomic::Ordering::Acquire) {
            MessageFutureInner::SEM_NOT_READY => {
                self.inner.waker.register(cx.waker());
                Poll::Pending
            }
            MessageFutureInner::SEM_READY => {
                self.inner
                    .completed
                    .store(MessageFutureInner::SEM_COMPLETED, atomic::Ordering::Release);
                Poll::Ready(unsafe { (&mut *self.inner.data.get()).assume_init_read() })
            }
            MessageFutureInner::SEM_COMPLETED => {
                panic!("{} already completed", core::any::type_name::<Self>())
            }
            // SAFETY: We definitely cant hit this branch
            _ => unsafe { core::hint::unreachable_unchecked() },
        }
    }
}

impl<T> futures_util::future::FusedFuture for MessageFuture<Receiver, T> {
    fn is_terminated(&self) -> bool {
        self.inner.completed.load(atomic::Ordering::Acquire) != MessageFutureInner::SEM_COMPLETED
    }
}

/*  SAFETY: This contains shared data and an ownership semaphore.
   There is a read and a write side. The semaphore indicates which side can modify the data.
   When the Sender sends the data the sender is also dropped, and the semaphore set do indicate the data is ready.
   The Receiver *only* reads the data when the semaphore indicates the data is ready
*/
unsafe impl<X: MessageDirection, T> Send for MessageFuture<X, T> where T: Send {}
unsafe impl<X: MessageDirection, T> Sync for MessageFuture<X, T> where T: Send {}
