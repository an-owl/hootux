use core::pin::Pin;
use core::task::{Context, Poll};
use futures_util::stream::Stream;
use conquer_once::spin::OnceCell;
use crossbeam_queue::ArrayQueue;
use futures_util::StreamExt;
use futures_util::task::AtomicWaker;
use pc_keyboard::{DecodedKey, HandleControl, Keyboard, layouts, ScancodeSet1};
use crate::{print, println};

static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();
static WAKER: AtomicWaker = AtomicWaker::new();

/// Called by the keyboard interrupt handler
///
/// Must not block or allocate.
pub(crate) fn add_scancode(scancode: u8) {
    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        if let  Err(_) = queue.push(scancode) {
            println!("WARNING: Scancode Queue full; dropping input")
        } else {
            WAKER.wake();
        }
    } else {
        println!("ERROR: Scancode queue not initialized")
    }
}

pub struct ScanCodeStream {
    _private: ()
}

impl ScanCodeStream{
    pub fn new() -> Self {
        SCANCODE_QUEUE.try_init_once(|| ArrayQueue::new(100))
            .expect("ScanCodeStream::new() should only be called once");
        ScanCodeStream{_private: ()}
    }
}

impl Stream for ScanCodeStream{
    type Item = u8;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let queue = SCANCODE_QUEUE.try_get()
            .expect("ERROR: SCANCODE_QUEUE not initialized");

        if let Some(scancode) = queue.pop(){
            return Poll::Ready(Some(scancode))
        }

        WAKER.register(&cx.waker());
        match queue.pop(){
            Some(scancode) => {
                WAKER.take();
                Poll::Ready(Some(scancode))
            }
            None => Poll::Pending
        }

    }
}

pub async fn print_key() {
    let mut scancodes = ScanCodeStream::new();
    let mut kb = Keyboard::new(
        layouts::Us104Key,
        ScancodeSet1,
        HandleControl::Ignore);

    while let Some(scancode) = scancodes.next().await {
        if let Ok(Some(key_event)) = kb.add_byte(scancode) {
            if let Some(key) = kb.process_keyevent(key_event) {
                match key {
                    DecodedKey::Unicode(char) => print!("{}", char),
                    DecodedKey::RawKey(key) => print!("{:?}", key)
                }
            }
        }
    }
}