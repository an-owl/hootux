use alloc::boxed::Box;
use alloc::string::ToString;
use core::pin::Pin;
use core::task::{Context, Poll};
use futures_util::future::BoxFuture;
use futures_util::{FutureExt};
use crate::derive_seek_blank;
use crate::fs::device::{Fifo, OpenMode};
use crate::serial::Serial;
use crate::fs::file::*;
use crate::fs::{IoError, IoResult};
use crate::fs::vfs::{DevID, MajorNum};
use crate::util::WritableBuffer;
use core::fmt::Write as _;
use core::marker::PhantomData;
use x86_64::instructions::interrupts::without_interrupts;

// fixme there is a bug in here somewhere causing stack overflows to occur
// I fixed it partially, it no longer panics the kernel but I'm not sure what the root cause is.

const DEFAULT_QUOTA_SIZE: usize = 4096;

lazy_static::lazy_static!(static ref MAJOR: MajorNum = MajorNum::new(););
static MINOR: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// This struct handles managing an instance of [Serial].
/// Its jobs include cleaning its outgoing buffers and handling asynchronously
/// waking tasks requesting to use the serial port.
///
/// This is the file object representing a [Serial] device.
#[derive(Clone)]
pub struct SerialDispatcher {
    inner: alloc::sync::Arc<SerialDispatcherInner>,
    fifo_lock: OpenMode,
    // This is a hack work-around.
    // We need a read-buffer but none can be provided using [crate::fs::device::Fifo::open].
}

struct SerialDispatcherInner {
    real: alloc::sync::Weak<Serial>,
    quota: atomic::Atomic<usize>,

    pend: spin::Mutex<alloc::collections::VecDeque<core::task::Waker>>,
    draining: atomic::Atomic<bool>,
    stream: futures_util::task::AtomicWaker,

    stream_lock: atomic::Atomic<bool>,

    id: DevID,
}

impl SerialDispatcher {
    pub(super) fn new(real: &alloc::sync::Arc<Serial>) -> Self {
        Self {
            inner: alloc::sync::Arc::new(SerialDispatcherInner {
                real: alloc::sync::Arc::downgrade(real),
                quota: atomic::Atomic::new(DEFAULT_QUOTA_SIZE),
                pend: Default::default(),
                draining: atomic::Atomic::new(false),
                stream: Default::default(),
                stream_lock: atomic::Atomic::new(false),

                id: DevID::new(*MAJOR,MINOR.fetch_add(1,atomic::Ordering::Relaxed))
            }),
            fifo_lock: Default::default(),
        }
    }

    pub(super) async fn run(self) -> crate::task::TaskResult {
        loop {
            if let Some(r) = self.inner.real.upgrade() {
                (&*r).await;

                // todo set a threshold for data out size. To prevent excess waking of the stream
                // check if the stream needs to be woken
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
                // Self.inner was dropped. This shouldn't happen normally. Maybe self was hot pluggable?
                // todo log actual info about why this was stopped
                return crate::task::TaskResult::StoppedExternally;
            }
        }
    }

    /// Don't use this if you can avoid it.
    /// It will push data to the serial buffer regardless of the quota always prefer to use the sink.
    /// This may break the ordering of the output.
    pub fn write_sync(&self, data: core::fmt::Arguments) -> Result<(),(IoError, usize)> {
        let mut self_mut = cast_file!(Fifo<u8>: self.clone_file()).unwrap();
        let st = data.to_string();
        self_mut.open(OpenMode::Write).map_err(|e| (e,0))?;

        crate::task::util::block_on!(self_mut.write(&st.as_bytes())).map(|_| ())
    }
}

#[cast_trait_object::dyn_upcast]
#[cast_trait_object::dyn_cast(NormalFile<u8>, Directory, crate::fs::device::FileSystem, crate::fs::device::Fifo<u8>, crate::fs::device::DeviceFile )]
impl File for SerialDispatcher {
    fn file_type(&self) -> FileType {
        FileType::CharDev
    }

    fn block_size(&self) -> u64 {
        1
    }

    fn device(&self) -> DevID {
        self.inner.id
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        0
    }

    fn len(&self) -> crate::fs::IoResult<u64> {
        // We make no expectation that any data is present
        async {
            Ok(0)
        }.boxed()
    }

    /// 0. Frame control. A unicode file representing the frame control formatted as a typical UART
    /// mode string e.g. "115200-8N1".
    /// Reads return the current mode.
    /// Reads will return no more than 10-bytes smaller buffers may return [IoError::EndOfFile].
    /// Writes must are given as a unicode string which, in order, consists of
    /// * The baud rate which must be given as an integer inclusive from "115200" and "1"
    /// * A single character ranging from 5 to 8
    /// * A Parity bit normally "N" (None). Accepted characters are NOMES.
    /// Definitions for these are out of the scope of this documentation
    /// * The number of stop bits either 1 or 2.
    ///
    ///
    fn b_file(&self, id: u64) -> Option<Box<dyn File>> {
        match id {
            0 => Some(Box::new(FrameCtlBFile{dispatch: self.clone()})), // frame control
            1 => todo!(), // rx-ringbuffer control
            _ => None,
        }
    }
}

impl Drop for SerialDispatcher {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

impl crate::fs::device::DeviceFile for SerialDispatcher {}

impl crate::fs::device::Fifo<u8> for SerialDispatcher {
    fn open(&mut self, mode: OpenMode) -> Result<(), IoError> {
        let _ = self.inner.real.upgrade().ok_or(IoError::MediaError)?; // assert that controller is still there

        if mode.is_write() {
            if let Err(_) = self.inner.stream_lock.compare_exchange_weak(false,true, atomic::Ordering::Acquire, atomic::Ordering::Relaxed) {
                return Err(IoError::Busy)
            }
        }

        self.fifo_lock = mode;
        Ok(())
    }

    fn close(&mut self) -> Result<(), IoError> {

        if self.fifo_lock == OpenMode::Locked {
            return Err(IoError::NotReady)
        }

        if self.fifo_lock.is_write() {
            self.inner.stream_lock.store(false,atomic::Ordering::Release);
        }
        self.fifo_lock = OpenMode::Locked;

        Ok(())
    }

    fn locks_remain(&self, mode: OpenMode) -> usize {
        if mode.is_write() {
            (!self.inner.stream_lock.load(atomic::Ordering::Relaxed)) as usize
        } else {
            usize::MAX
        }
    }

    fn is_master(&self) -> Option<usize> {
        None
    }
}

impl Read<u8> for SerialDispatcher {
    fn read<'a>(&'a mut self, buff: &'a mut [u8]) -> BoxFuture<Result<&'a mut [u8], (IoError, usize)>> {
        if self.fifo_lock.is_read() {
            // This looks strange. ReadFut::poll() queries if any data has become available
            let real = if let Some(real) = self.inner.real.upgrade() {
                real
            } else {
                return async {Err((IoError::MediaError,0)) }.boxed()
            };

            let mut l = real.rx_tgt.lock();

            if l.is_some() {
                return async { Err((IoError::Busy, 0)) }.boxed();
            }
            *l = Some((buff as *mut [u8],0));
            ReadFut {
                dispatch: self,
                phantom_buffer: PhantomData,
            }.boxed()
        } else {
            async { Err((IoError::NotReady, 0)) }.boxed()
        }
    }
}

/// Future for reading from serial port.
///
/// When this is `await`ed, this will check a number of conditions and return values depending on their result.
///
/// 1. If the buffer has been filled then `Poll::Ready(_)` is returned.
/// 2. If data is currently being received this will return `Poll::Pending`
/// 3. If no data has been written to the buffer then `Poll::Pending` is returned.
/// 4. If data has been received and the line is currently idle this will return `Poll::Ready(_)` with an incomplete buffer.
struct ReadFut<'a,'b> {
    dispatch: &'a SerialDispatcher,
    phantom_buffer: PhantomData<&'b mut [u8]>
}

impl<'a,'b> core::future::Future for ReadFut<'a,'b> {
    type Output = Result<&'b mut [u8],(IoError,usize)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let real = if self.dispatch.fifo_lock.is_read() {
            self.dispatch.inner.real.upgrade().ok_or((IoError::MediaError, 0))?
        } else {
            Err((IoError::NotReady, 0))?
        };


        without_interrupts(|| {
            let mut l = real.rx_tgt.lock();
            let (ref buff, ref i) = *(*l).as_ref().unwrap();

            return if *i == buff.len() {
                Poll::Ready(Ok(unsafe { &mut *l.take().unwrap().0 }))

            } else if real.rx_idle.load(atomic::Ordering::Relaxed) && *i > 0 {
                let len = *i;
                Poll::Ready(Ok( unsafe { &mut (*l.take().unwrap().0)[..len] }))

            } else {
                self.dispatch.inner.stream.register(cx.waker());
                Poll::Pending
            }
        })
    }
}

impl Write<u8> for SerialDispatcher {
    fn write<'a>(&'a mut self, buff: &'a [u8]) -> BoxFuture<Result<usize, (IoError, usize)>> {
        async {
            if self.fifo_lock.is_write() {
                // Returning here indicates that the driver has closed the controller.
                let real = self.inner.real.upgrade().ok_or((IoError::MediaError, 0))?;

                let run = real.run.swap(true,atomic::Ordering::Acquire);
                let mut write_buff = real.write_buff.lock();
                let push = write_buff.push(buff);

                if !run {
                    without_interrupts(|| {
                        if let Some(b) = write_buff.pop() {
                            real.try_send(b).unwrap(); // Should never be None
                        }
                        drop(write_buff);
                    });
                }
                push.await;

                Ok(buff.len())
            } else {
                Err((IoError::NotReady,0))
            }
        }.boxed()
    }
}

#[derive(Clone)]
#[cast_trait_object::dyn_upcast(File)]
#[cast_trait_object::dyn_cast(File => NormalFile<u8>, Directory, crate::fs::device::FileSystem, crate::fs::device::Fifo<u8>, crate::fs::device::DeviceFile )]
struct FrameCtlBFile {
    dispatch: SerialDispatcher
}

impl File for FrameCtlBFile {
    fn file_type(&self) -> FileType {
        FileType::NormalFile
    }

    fn block_size(&self) -> u64 {
        crate::mem::PAGE_SIZE as u64
    }

    fn device(&self) -> DevID {
        self.dispatch.inner.id
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        0
    }

    fn len(&self) -> IoResult<u64> {
        async { Ok(crate::mem::PAGE_SIZE as u64) }.boxed()
    }
}

impl NormalFile for FrameCtlBFile {
    fn len_chars(&self) -> IoResult<u64> {
        async { Ok(crate::mem::PAGE_SIZE as u64) }.boxed()
    }

    fn file_lock<'a>(self: Box<Self>) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
        async {Err((IoError::NotSupported,self as Box<dyn NormalFile>))}.boxed()
    }

    unsafe fn unlock_unsafe(&self) -> IoResult<()> {
        async {Err(IoError::NotSupported)}.boxed()
    }
}

impl Read<u8> for FrameCtlBFile {
    fn read<'a>(&'a mut self, buff: &'a mut [u8]) -> BoxFuture<Result<&'a mut [u8], (IoError, usize)>> {
        async {
            let real = self.dispatch.inner.real.upgrade().ok_or((IoError::MediaError,0))?;
            let b_rate = ((real.divisor.load(atomic::Ordering::Relaxed) as f32)/115200f32) as u32; // use emulated float for conversion to baud-rate
            let data_bits: u8 = real.bits.load(atomic::Ordering::Relaxed).into();
            let parity: char = real.parity.load(atomic::Ordering::Relaxed).into();
            let stop = real.stop.load(atomic::Ordering::Relaxed) as u8 + 1;

            // 10 bytes is the most we write we do this to determine how many bytes we wrote.
            // also moves buff[0..10] into L1D
            let end = buff.len().min(10);
            buff[..end].fill(0);
            let err = write!(buff.writable(),"{b_rate}-{data_bits}{parity}{stop}").is_err();
            if err {
                return Err((IoError::EndOfFile,buff.len()))
            }

            let len = buff[..10].iter().position(|c| *c == 0).unwrap_or(10);
            Ok(&mut buff[..len])
        }.boxed()
    }
}

derive_seek_blank!(FrameCtlBFile);

impl Write<u8> for FrameCtlBFile {
    fn write<'a>(&'a mut self, buff: &'a [u8]) -> BoxFuture<Result<usize, (IoError, usize)>> {
        async {
            let s = core::str::from_utf8(buff).map_err(|_| (IoError::InvalidData, 0))?;
            let (baud_rate, frame) = s.split_at(s.find('-').ok_or((IoError::InvalidData, 0))?);

            let baud_rate: u32 = baud_rate.parse().map_err(|_| (IoError::InvalidData, 0))?;
            if baud_rate > 115200 {
                return Err((IoError::InvalidData,0))
            }

            let divisor: u16 = ((115200f32 / baud_rate as f32) as u32).try_into().unwrap();
            if frame.len() != 4 {
                return Err((IoError::InvalidData,0))
            }
            let frame_fmt: [char;3] = {
                let mut f = frame.bytes().map(|b| b as char);
                let r = [f.next().ok_or((IoError::InvalidData,0))?,f.next().ok_or((IoError::InvalidData,0))?,f.next().ok_or((IoError::InvalidData,0))?];
                if let Some(_) = f.next() {
                    return Err((IoError::InvalidData,0))
                }
                r
            };
            let data_bits = match frame_fmt[0] {
                '5' => super::DataBits::Five,
                '6' => super::DataBits::Six,
                '7' => super::DataBits::Seven,
                '8' => super::DataBits::Eight,
                _ => return Err((IoError::InvalidData,0))
            };

            let parity = match frame_fmt[1].to_ascii_uppercase() {
              // G
                'N' => super::Parity::None,
                'O' => super::Parity::Odd,
                'M' => super::Parity::Mark,
                'E' => super::Parity::Even,
                'S' => super::Parity::Space,
                _ => return Err((IoError::InvalidData,0))
            };

            let stop_bits = match frame_fmt[2] {
                '1' => super::StopBits::One,
                '2' => super::StopBits::Two,
                _ => return Err((IoError::InvalidData,0))
            };

            let real = self.dispatch.inner.real.upgrade().ok_or((IoError::MediaError,0))?;
            real.set_char_mode(data_bits,parity,stop_bits);
            real.set_divisor(divisor);
            Ok(buff.len())
        }.boxed()
    }
}

/*

#[derive(Clone)]
#[cast_trait_object::dyn_upcast(File)]
#[cast_trait_object::dyn_cast(File => NormalFile<u8>, Directory, crate::fs::device::FileSystem, crate::fs::device::Fifo<u8>, crate::fs::device::DeviceFile )]
struct RingbuffCtlBFile {
    inner: SerialDispatcher
}

impl File for RingbuffCtlBFile {
    fn file_type(&self) -> FileType {
        FileType::NormalFile
    }

    fn block_size(&self) -> u64 {
        crate::mem::PAGE_SIZE as u64
    }

    fn device(&self) -> DevID {
        self.inner.inner.id
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        0
    }

    fn len(&self) -> IoResult<u64> {
        async { Ok(crate::mem::PAGE_SIZE as u64) }.boxed()
    }
}

impl NormalFile for RingbuffCtlBFile {
    fn len_chars(&self) -> IoResult<u64> {
        async {Ok(crate::mem::PAGE_SIZE as u64)}.boxed()
    }

    fn file_lock<'a>(self: Box<Self>) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
        async { Err((IoError::NotSupported, self as Box<dyn NormalFile>)) }.boxed()
    }

    unsafe fn unlock_unsafe(&self) -> IoResult<()> {
        async {Err(IoError::NotSupported)}.boxed()
    }
}

derive_seek_blank!(RingbuffCtlBFile);

impl Read<u8> for RingbuffCtlBFile {
    fn read<'a>(&'a mut self, buff: &'a mut [u8]) -> BoxFuture<Result<&'a mut [u8], (IoError, usize)>> {
        async {
            // If you modify this fn then ensure that `write!` never returns Err(_)

            let mut stack_buff = [0u8; 8];
            let real = self.inner.inner.real.upgrade().ok_or((IoError::MediaError, 0))?;
            let len = real.read_buff.read().as_ref().map_or(0,|b| b.len());

            let _ = write!(stack_buff.writable(),"{}",len); // will never fail
            let pos = stack_buff.iter().position(|c| *c == 0).unwrap(); // will never be null

            let end = pos.min(buff.len());
            buff[0..end].copy_from_slice(&stack_buff[0..pos]);
            if buff.len() < pos {
                Err((IoError::EndOfFile,pos))
            } else {
                Ok(&mut buff[..pos])
            }
        }.boxed()
    }
}

impl Write<u8> for RingbuffCtlBFile {
    fn write<'a>(&'a mut self, buff: &'a [u8]) -> BoxFuture<Result<usize, (IoError, usize)>> {
        async {
            let real  = self.inner.inner.real.upgrade().ok_or((IoError::MediaError,0))?;
            let l = real.read_buff.upgradeable_read();
            let n_len: u16 = core::str::from_utf8(buff).map_err(|_| (IoError::InvalidData,0))?.parse().map_err(|_| (IoError::InvalidData,0))?;

            // Buffer must be empty before we can modify it.
            if l.as_ref().map_or(true,|b| b.is_empty()) {

                let queue;
                if n_len == 0 {
                    queue = None;
                } else {
                    queue = Some(crossbeam_queue::ArrayQueue::new(n_len as usize));
                }

                x86_64::instructions::interrupts::without_interrupts(||
                    {
                        let mut l = l.upgrade();
                        *l = queue;
                    }
                );
                Ok(buff.len())
            } else {
                Err((IoError::NotReady,0))
            }
        }.boxed()
    }
}

 */