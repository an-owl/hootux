/// Buffer which can be used to send data to be used during interrupts.
/// Data is stored in a series of chunks, when new data is appended it can allocate a new chunk.
/// Chunks will not be freed implicitly, will ned to be freed implicitly, to prevent UB during interrupts.

pub struct ChonkyBuff<T: Copy> {
    inner: spin::Mutex<ChonkyBuffInner<T>>,
    /// Wakes a task specifically to call `free` on self.
    cleaner: futures_util::task::AtomicWaker,
    /// Wakes the task to notify it that the buffer has been drained and requires more data.
    task: futures_util::task::AtomicWaker,

    len: core::sync::atomic::AtomicUsize,
    invd_len: core::sync::atomic::AtomicUsize,
}

struct ChonkyBuffInner<T: Copy> {
    chonk: alloc::collections::LinkedList<alloc::boxed::Box<[T]>>,
    comp_chonk: alloc::collections::LinkedList<alloc::boxed::Box<[T]>>,
    chonk_offset: usize,
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
            task: futures_util::task::AtomicWaker::new(),
            len: core::sync::atomic::AtomicUsize::new(0),
            invd_len: core::sync::atomic::AtomicUsize::new(0),
        }
    }

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

    pub fn push<U: Into<alloc::boxed::Box<[T]>>>(&mut self, data: U) {
        let b = data.into();
        self.free();

        // skip that shit bruh
        if b.len() == 0 {
            log::trace!(
                "Tried to push buffer of size 0 to {}",
                core::any::type_name::<Self>()
            );
            return;
        }

        self.len.fetch_add(b.len(), atomic::Ordering::Acquire);

        let mut n = alloc::collections::LinkedList::new();
        n.push_back(b);

        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut l = self.inner.lock();
            l.chonk.append(&mut n);
        })
    }

    pub fn pop(&self) -> Option<T> {
        let mut l = self.inner.lock();
        // fetch T
        let ch = l.chonk.front();
        if ch.is_none() {
            self.task.wake();
        }
        let ch = ch?;

        if let Some(_) = ch.get(l.chonk_offset) {
            l.chonk_offset += 1;
            return Some(l.chonk.front()?[l.chonk_offset - 1]);
        } else {
            let mut bind = l.chonk.split_off(0);
            // we want to remove the first element not the rest
            core::mem::swap(&mut bind, &mut l.chonk);
            l.comp_chonk.append(&mut bind);
            self.invd_len
                .fetch_add(bind.back()?.len(), atomic::Ordering::Release);

            // re-tries fetching
            l.chonk_offset = 0;
            self.cleaner.wake();
            let r = l.chonk.front()?[0];
            l.chonk_offset += 1;
            Some(r)
        }
    }

    /// Peeks the next `T` That will be returned by [Self::pop].
    ///
    /// This fn will invalidate the current chunk if it is completed.
    pub fn peek(&self) -> Option<T> {
        let mut l = self.inner.lock();

        if let Some(r) = l.chonk.front()?.get(l.chonk_offset) {
            return Some(*r);
        } else {
            let mut bind = l.chonk.split_off(0);
            core::mem::swap(&mut bind, &mut l.chonk);
            l.comp_chonk.append(&mut bind);
            self.invd_len
                .fetch_add(bind.back()?.len(), atomic::Ordering::Release);

            // re-tries fetching
            l.chonk_offset = 0;
            self.cleaner.wake();
            let r = l.chonk.front()?[0];
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
