use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

/// Buffer which can be used to send data to be used during interrupts.
/// Data is stored in a series of chunks, when new data is appended it can allocate a new chunk.
/// Chunks will not be freed implicitly, will need to be freed implicitly, to prevent UB during interrupts.

pub struct ChonkyBuff<T: Copy> {
    /// Contains waker for the task that handed over the data.
    inner: spin::Mutex<ChonkyBuffInner<T>>,
    /// Wakes a task specifically to call `free` on self.
    cleaner: futures_util::task::AtomicWaker,
    /// Wakes the task to notify it that the buffer has been drained and requires more data.
    manager_waker: futures_util::task::AtomicWaker,

    /// number of pending chunks
    len: core::sync::atomic::AtomicUsize,
    /// number of completed chunks
    invd_len: core::sync::atomic::AtomicUsize,
}

unsafe impl<T: Copy> Send for ChonkyBuff<T> where T: Send {}
unsafe impl<T: Copy> Sync for ChonkyBuff<T> where T: Sync {}

struct ChonkyBuffInner<T: Copy> {
    chonk: alloc::collections::LinkedList<alloc::sync::Arc<BufferChonk<T>>>,
    // completed
    //comp_chonk: alloc::collections::LinkedList<alloc::sync::Arc<BufferChonk<T>>>,
    comp_chonk: alloc::collections::LinkedList<alloc::sync::Arc<BufferChonk<T>>>,
    chonk_offset: usize,
}

struct BufferChonk<T> {
    waker: futures_util::task::AtomicWaker,
    completed: atomic::Atomic<bool>,
    // SAFETY: Reads from chonk are safe, it can only be constructed from a reference, and is a raw pointer to erase the lifetime
    chonk: *const [T],
}

impl<T: Copy> ChonkyBuff<T> {
    pub const fn new() -> Self {
        Self {
            inner: spin::Mutex::new(ChonkyBuffInner {
                chonk: alloc::collections::LinkedList::new(),
                comp_chonk: alloc::collections::LinkedList::new(),
                chonk_offset: 0,
            }),
            cleaner: futures_util::task::AtomicWaker::new(),
            manager_waker: futures_util::task::AtomicWaker::new(),
            len: core::sync::atomic::AtomicUsize::new(0),
            invd_len: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Frees the contents of comp_chonk
    pub fn free(&mut self) {
        let mut bind = alloc::collections::LinkedList::new();

        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut l = self.inner.lock();

            // moves all completed chunks into `bind`
            core::mem::swap(&mut l.comp_chonk, &mut bind);

            self.len.fetch_sub(
                self.invd_len.swap(0, atomic::Ordering::Release),
                atomic::Ordering::Release,
            );
        });

        drop(bind);
    }

    pub fn push<'b, 'a: 'b>(&'b mut self, data: &'a [T]) -> Push<'a, T> {
        let b = alloc::sync::Arc::new(BufferChonk {
            waker: Default::default(),
            completed: atomic::Atomic::new(false),
            chonk: data,
        });
        self.free();

        // skip that shit bruh
        if b.chonk.len() == 0 {
            log::trace!(
                "Tried to push buffer of size 0 to {}",
                core::any::type_name::<Self>()
            );
            // SAFETY: This is safe because the slice has a len of 0
            return Push {
                phantom_data: Default::default(),
                buff: alloc::sync::Arc::new(BufferChonk {
                    waker: Default::default(),
                    completed: atomic::Atomic::new(true),
                    chonk: unsafe { core::slice::from_raw_parts(core::ptr::null(), 0) },
                }),
            };
        }

        self.len.fetch_add(b.chonk.len(), atomic::Ordering::Acquire);

        let mut n = alloc::collections::LinkedList::new();
        n.push_back(b.clone());

        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut l = self.inner.lock();
            l.chonk.append(&mut n);
        });
        Push {
            phantom_data: Default::default(),
            buff: b,
        }
    }

    pub fn pop(&self) -> Option<T> {
        let mut l = self.inner.lock();
        // fetch T
        let ch = l.chonk.front();
        if ch.is_none() {
            self.manager_waker.wake();
        }
        let ch = ch?;

        if let Some(_) = unsafe { &*ch.chonk }.get(l.chonk_offset) {
            l.chonk_offset += 1;
            Some(unsafe { (&*l.chonk.front()?.chonk)[l.chonk_offset - 1] })
        } else {
            let mut bind = l.chonk.split_off(1);
            // we want to pop the first element not the rest
            core::mem::swap(&mut bind, &mut l.chonk);
            let completed = bind.front().unwrap();
            completed.waker.wake();
            completed.completed.store(true, atomic::Ordering::Release);

            let rm_len = bind.back().unwrap().chonk.len();
            l.comp_chonk.append(&mut bind);
            self.invd_len.fetch_add(rm_len, atomic::Ordering::Release);

            // re-tries fetching
            l.chonk_offset = 0;
            self.cleaner.wake();
            let r = unsafe { (&*l.chonk.front()?.chonk)[0] };
            l.chonk_offset += 1;

            Some(r)
        }
    }

    /// Peeks the next `T` That will be returned by [Self::pop].
    ///
    /// This fn will invalidate the current chunk if it is completed.
    pub fn peek(&self) -> Option<T> {
        let mut l = self.inner.lock();

        if let Some(r) = unsafe { &*l.chonk.front()?.chonk }.get(l.chonk_offset) {
            Some(*r)
        } else {
            let mut bind = l.chonk.split_off(0);
            core::mem::swap(&mut bind, &mut l.chonk);
            l.comp_chonk.append(&mut bind);
            self.invd_len
                .fetch_add(bind.back()?.chonk.len(), atomic::Ordering::Release);

            // re-tries fetching
            l.chonk_offset = 0;
            self.cleaner.wake();
            let r = unsafe { (&*l.chonk.front()?.chonk)[0] };
            Some(r)
        }
    }

    /// Returns the current size of `self`
    ///
    /// This can be used to enforce size quotas.
    pub fn len(&self) -> usize {
        self.len.load(atomic::Ordering::Relaxed)
    }

    /// Returns the amount of valid data contained within `self`
    pub fn valid_len(&self) -> usize {
        self.len.load(atomic::Ordering::Relaxed) - self.invd_len.load(atomic::Ordering::Relaxed)
    }
}

pub struct Push<'a, T> {
    phantom_data: PhantomData<&'a [T]>,
    buff: alloc::sync::Arc<BufferChonk<T>>,
}

impl<T> core::future::Future for Push<'_, T> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.buff.completed.load(atomic::Ordering::Relaxed) {
            Poll::Ready(())
        } else {
            self.buff.waker.register(&cx.waker());
            Poll::Pending
        }
    }
}

// SAFETY: This is safe because the shared data is never accessed
unsafe impl<T> Send for Push<'_, T> where T: Send {}
unsafe impl<T> Sync for Push<'_, T> where T: Sync {}
