use alloc::boxed::Box;
use core::pin::Pin;
use core::task::{Context, Poll};

const DEFAULT_QUOTA_SIZE: usize = 4096;

/// This struct handles managing an instance of [super::Serial].
/// Its jobs include cleaning its outgoing buffers and handling asynchronously
/// waking tasks requesting to use the serial port.
///
/// SerialPortDispatcher implements [futures_util::Sink] for `Box<[u8]>` is implemented for all
/// types which implement `Into<String>` or `Into<&str>`
#[derive(Clone)]
pub struct SerialDispatcher {
    inner: alloc::sync::Arc<SerialDispatcherInner>,
}

struct SerialDispatcherInner {
    real: alloc::sync::Weak<super::Serial>,
    quota: atomic::Atomic<usize>,

    pend: spin::Mutex<alloc::collections::VecDeque<core::task::Waker>>,
    draining: atomic::Atomic<bool>,
    sync: spin::Mutex<alloc::collections::VecDeque<core::task::Waker>>,
    stream: futures_util::task::AtomicWaker,

    stream_lock: atomic::Atomic<bool>,
}

impl SerialDispatcher {
    pub(super) fn new(real: &alloc::sync::Arc<super::Serial>) -> Self {
        Self {
            inner: alloc::sync::Arc::new(SerialDispatcherInner {
                real: alloc::sync::Arc::downgrade(real),
                quota: atomic::Atomic::new(DEFAULT_QUOTA_SIZE),
                pend: Default::default(),
                draining: atomic::Atomic::new(false),
                sync: Default::default(),
                stream: Default::default(),
                stream_lock: atomic::Atomic::new(false),
            }),
        }
    }

    /// Attempts to lock the read stream, returns `None` if the stream is already locked.
    pub fn lock_stream(&self, buffer: Box<[u8]>) -> Option<SerialStream> {
        self.inner
            .stream_lock
            .compare_exchange_weak(
                false,
                true,
                atomic::Ordering::Acquire,
                atomic::Ordering::Relaxed,
            )
            .ok()?;

        let real = self.inner.real.upgrade()?;
        x86_64::instructions::interrupts::without_interrupts(|| {
            *real.read_buff.lock() = Some((0, buffer))
        });

        Some(SerialStream {
            dispatcher: alloc::sync::Arc::downgrade(&self.inner),
            dyn_queue: Default::default(),
        })
    }

    pub(super) async fn run(self) {
        loop {
            if let Some(r) = self.inner.real.upgrade() {
                (&*r).await;

                // todo set a threshold for data out size. To prevent excess waking of the stream
                // check if the stream needs to be woken
                if x86_64::instructions::interrupts::without_interrupts(|| {
                    r.read_buff.lock().as_ref().map_or(0, |n| n.0)
                }) > 0
                {
                    self.inner.stream.wake();
                }

                x86_64::instructions::interrupts::without_interrupts(|| {
                    let mut l = r.write_buff.lock();

                    if l.len() > l.valid_len() {
                        l.free();
                        if l.len() < self.inner.quota.load(atomic::Ordering::Relaxed) {
                            if !self.inner.draining.swap(true, atomic::Ordering::Acquire) {
                                // waiting tasks will wake the next on if it can.
                                // calling wake may wake the wrong task
                                // so this checks if the wakers are currently being woken
                                self.inner.pend.lock().pop_front().map(|d| d.wake());
                            }
                        }
                    } else {
                        // free must be called regardless
                        l.free();
                    }
                });
            } else {
                // this allows self.parent to be dropped if this is the only reference to it.
                return;
            }
        }
    }

    /// Don't use this if you can avoid it.
    /// It will push data to the serial buffer regardless of the quota always prefer to use the sink.
    /// This may break the ordering of the output.
    pub fn write_sync<T: Into<Box<[u8]>>>(&self, data: T) -> Result<(), SinkErr> {
        let r = self.inner.real.upgrade().ok_or(SinkErr::Closed)?;
        x86_64::instructions::interrupts::without_interrupts(|| {
            r.write_buff.lock().push(data.into());
            if r.run
                .compare_exchange_weak(
                    false,
                    true,
                    atomic::Ordering::Relaxed,
                    atomic::Ordering::Relaxed,
                )
                .is_ok()
            {
                if let Some(b) = r.write_buff.lock().pop() {
                    r.try_send(b)
                        .expect("Failed to send data but Serial.run was false");
                }
            }
        });

        Ok(())
    }
}

impl<T: Into<Box<[u8]>>> futures_util::Sink<T> for SerialDispatcher {
    type Error = SinkErr;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Some(r) = self.inner.real.upgrade() {
            // to maintain strong ordering if other tasks are waiting to write then this must be blocked
            if self.inner.quota.load(atomic::Ordering::Relaxed) < r.write_buff.lock().len()
                && self.inner.pend.lock().len() == 0
            {
                return Poll::Ready(Ok(()));
            }

            self.inner.pend.lock().push_back(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(Err(SinkErr::Closed))
        }
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        let r = self.inner.real.upgrade().ok_or(SinkErr::Closed)?;
        r.queue_send(item.into());
        if x86_64::instructions::interrupts::without_interrupts(|| r.write_buff.lock().len())
            < self.inner.quota.load(atomic::Ordering::Release)
        {
            self.inner
                .pend
                .lock()
                .pop_front()
                .and_then(|w| Some(w.wake()));
        } else {
            self.inner.draining.store(false, atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    /// This is a pain, if this is woken then all data written before this was called has been flushed.
    /// If polling this a second time returns [Poll::Pending] then more data has been added
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Some(r) = self.inner.real.upgrade() {
            x86_64::instructions::interrupts::without_interrupts(|| {
                return if r.write_buff.lock().len() == 0 {
                    Poll::Ready(Ok(()))
                } else {
                    self.inner.sync.lock().push_back(cx.waker().clone());
                    Poll::Pending
                };
            })
        } else {
            return Poll::Ready(Err(SinkErr::Closed));
        }
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Err(SinkErr::CannotClose))
    }
}

#[derive(Copy, Clone, Debug)]
pub enum SinkErr {
    CannotClose,
    Closed,
}

/// SerialStream handles a stream being sent from a serial port.
/// SerialStream is constructed from the [SerialDispatcherInner::lock_stream] method this method gives a
/// buffer to the serial driver to be written to asynchronously during interrupts.
/// If the buffer overflows then data will be lost.
/// It is the responsibility of the caller to ensure the buffer does not overflow.
pub struct SerialStream {
    dispatcher: alloc::sync::Weak<SerialDispatcherInner>,
    dyn_queue: alloc::collections::VecDeque<u8>,
}

impl SerialStream {
    fn get_real(&self) -> Option<alloc::sync::Arc<super::Serial>> {
        self.dispatcher.upgrade()?.real.upgrade()
    }
}

impl futures_util::Stream for SerialStream {
    type Item = u8;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(r) = self.get_real() {
            x86_64::instructions::interrupts::without_interrupts(|| {
                let mut l = r.read_buff.lock();
                let (len, b) = l.as_mut().unwrap();
                self.dyn_queue.extend(b[..*len].iter());
                *len = 0;
            })
        }

        if let Some(b) = self.dyn_queue.pop_front() {
            Poll::Ready(Some(b))
        } else {
            if let Some(d) = self.dispatcher.upgrade() {
                d.stream.register(cx.waker());
                Poll::Pending
            } else {
                Poll::Ready(None)
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.dyn_queue.len(), None)
    }
}

impl Drop for SerialStream {
    fn drop(&mut self) {
        if let Some(d) = self.dispatcher.upgrade() {
            if let Some(r) = d.real.upgrade() {
                x86_64::instructions::interrupts::without_interrupts(|| r.read_buff.lock().take());
            }

            d.stream_lock.store(false, atomic::Ordering::Release);
        }
    }
}
