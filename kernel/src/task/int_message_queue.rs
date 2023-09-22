use core::pin::Pin;
use core::task::{Context, Poll};
use futures_util::{Stream, StreamExt};

/// IntMessageQueue is used to handle messages delivered via interrupt. Messages definitions are
/// defined by `T`.
pub struct IntMessageQueue<T: MessageCfg> {
    interrupts: alloc::vec::Vec<crate::interrupts::InterruptIndex>,
    queue: super::InterruptQueue,
    cfg: T,
}

impl<T: MessageCfg> IntMessageQueue<T> {
    /// Creates a new instance of self
    ///
    /// call [super::new_int_queue] to create a queue
    fn new(
        queue: super::InterruptQueue,
        irqs: alloc::vec::Vec<crate::interrupts::InterruptIndex>,
        cfg: T,
    ) -> Self {
        Self {
            interrupts: irqs,
            queue,
            cfg,
        }
    }

    /// Frees all IRQs owned by `self`. This must be called prior to dropping `self`.
    ///
    /// # Safety
    ///
    /// The caller must ensure that any IRQs managed by `self` will not be raised after this fn is called.
    /// The caller may **not** do this globally masking interrupts.
    pub unsafe fn drop_irq(&mut self) {
        while let Some(i) = self.interrupts.pop() {
            crate::interrupts::free_irq(i).expect("Double free IRQ");
        }
    }

    /// Polls the queue and returns the message along with the number of messages in the buffer
    /// (including the one just polled).
    pub async fn poll_with_count(mut self: Pin<&mut Self>) -> (super::InterruptMessage, usize) {
        let r = self.next().await.unwrap(); // The must never return Ready(None)
        let rem = self.queue.len() + 1;
        (r, rem)
    }
}

impl IntMessageQueue<crate::system::pci::CfgIntResult> {
    pub fn from_pci(dev: &mut crate::system::pci::DeviceControl, size: usize) -> Option<Self> {
        let queue = super::new_int_queue(size);
        // SAFETY: This is safe because override_count is None
        let (cfg, irqs) = unsafe { dev.cfg_interrupts(queue.clone(), None) };
        debug_assert!(
            cfg.success() == irqs.is_some(),
            "pci::DeviceControl::cfg_interrupts returned invalid results"
        );
        Some(Self {
            interrupts: irqs?,
            queue,
            cfg,
        })
    }
}

impl TryFrom<(&mut crate::system::pci::DeviceControl, usize)>
    for IntMessageQueue<crate::system::pci::CfgIntResult>
{
    type Error = ();
    fn try_from(
        value: (&mut crate::system::pci::DeviceControl, usize),
    ) -> Result<Self, Self::Error> {
        IntMessageQueue::from_pci(value.0, value.1).ok_or(())
    }
}

impl<T: MessageCfg> Stream for IntMessageQueue<T> {
    type Item = super::InterruptMessage;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.queue.pop() {
            Some(m) => Poll::Ready(Some(m)),
            None => {
                for i in &self.interrupts {
                    crate::interrupts::reg_waker(*i, cx.waker())
                        .expect("Tried to register IntStatMachine to non Generic handler");
                }
                Poll::Pending
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.queue.len(), Some(self.queue.capacity()))
    }
}

impl<T: MessageCfg> Drop for IntMessageQueue<T> {
    fn drop(&mut self) {
        if !self.interrupts.is_empty() {
            panic!(
                "Attempted to drop {} without freeing IRQs",
                core::any::type_name::<Self>()
            );
        }
    }
}

/// Allows implementors to describe interrupt messages for interrupt vectors managed by implementors.
///
/// Libraries implementing this trait should also implement  
impl<T: MessageCfg> MessageCfg for IntMessageQueue<T> {
    fn count(&self) -> usize {
        self.cfg.count()
    }

    fn message(&self, vector: usize) -> super::InterruptMessage {
        self.cfg.message(vector)
    }
}

pub trait MessageCfg {
    /// Returns the number of requester vectors managed by self.
    /// The number of vectors may not be the same as the number of IRQs used.
    fn count(&self) -> usize;

    /// Returns the message returned by the given vector.
    ///
    /// # Panics
    ///
    /// implementors should panic if `vector` is greater than `self.count()`
    fn message(&self, vector: usize) -> super::InterruptMessage;
}
