use crate::fs::device::{Fifo, OpenMode};
use crate::fs::file::*;
use crate::fs::vfs::{DevID, MajorNum};
use crate::fs::{IoError, IoResult};
use crate::mem::dma::DmaBuff;
use crate::serial::Serial;
use crate::util::ToWritableBuffer;
use alloc::boxed::Box;
use alloc::string::ToString;
use core::any::Any;
use core::fmt::Write as _;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use kernel_proc_macro::ok_or_lazy;
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
#[kernel_proc_macro::file]
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

impl crate::fs::sysfs::SysfsFile for SerialDispatcher {}

impl crate::fs::sysfs::bus::BusDeviceFile for SerialDispatcher {
    fn bus(&self) -> &'static str {
        "uart"
    }

    fn id(&self) -> alloc::string::String {
        alloc::format!("uart{}", self.inner.id.as_int().1).to_string()
    }

    fn as_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
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

                id: DevID::new(*MAJOR, MINOR.fetch_add(1, atomic::Ordering::Relaxed)),
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
    pub fn write_sync(&self, data: core::fmt::Arguments) -> Result<(), (IoError, usize)> {
        hootux::block_on!(self.write(0, data.to_string().into()))
            .map_err(|(e, _, len)| (e, len))?;
        Ok(())
    }

    fn set_mode(
        &self,
        data_bits: &super::DataBits,
        parity: &super::Parity,
        stop_bits: &super::StopBits,
    ) {
        let Some(real) = self.inner.real.upgrade() else {
            return;
        };
        real.set_char_mode(*data_bits, *parity, *stop_bits)
    }
}

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

    fn len(&self) -> crate::fs::IoResult<'_, u64> {
        // We make no expectation that any data is present
        async { Ok(0) }.boxed()
    }

    fn method_call<'f, 'a: 'f, 'b: 'f>(
        &'b self,
        method: &str,
        arguments: &'a (dyn Any + Send + Sync + 'a),
    ) -> IoResult<'f, MethodRc> {
        kernel_proc_macro::impl_method_call!(method, arguments =>
            async set_mode(super::DataBits, super::Parity, super::StopBits)
        )
    }

    /// 0. Frame control see [FrameCtlBFile]
    /// Definitions for these are out of the scope of this documentation
    /// * The number of stop bits either 1 or 2.
    ///
    ///
    fn b_file(&self, id: u64) -> Option<Box<dyn File>> {
        match id {
            0 => Some(Box::new(FrameCtlBFile {
                dispatch: self.clone(),
            })), // frame control
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

        if mode.is_read() {
            if let Err(_) = self.inner.stream_lock.compare_exchange_weak(
                false,
                true,
                atomic::Ordering::Acquire,
                atomic::Ordering::Relaxed,
            ) {
                return Err(IoError::Busy);
            }
        }

        self.fifo_lock = mode;
        Ok(())
    }

    fn close(&mut self) -> Result<(), IoError> {
        if self.fifo_lock == OpenMode::Locked {
            return Err(IoError::NotReady);
        }

        if self.fifo_lock.is_write() {
            self.inner
                .stream_lock
                .store(false, atomic::Ordering::Release);
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

/// This trait's methods must check and configure beforehand the controller to receive data.
/// This may result in the buffer being partially or fully read before the future is returned.
///
/// When this function is called the read is initialized. Data will be read between when a generator
/// function is called until this returns [Poll::Ready]. The future returned by this function will
/// be woken once data has been received not when the buffer has been filled. A caller may wish to
/// call [Read::read] and wait on a timeout instead.
///
/// QEMU uses an 8250 implementation however due to host file handling Rx is buffered regardless.
///
/// Note: At the time of writing timers are only accurate to 4ms.
impl Read<u8> for SerialDispatcher {
    fn read(
        &self,
        _: u64,
        mut dbuff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async {
            if self.fifo_lock.is_read() {
                // cant use ok_or(_) here because of use after free on `dbuff`
                let real = match self.inner.real.upgrade() {
                    Some(real) => real,
                    None => return Err((IoError::MediaError, dbuff, 0)),
                };

                let buff = &mut *dbuff;

                // Interrupts must be blocked here to prevent deadlocks.
                // returning Some(_) here indicates early completion.
                match without_interrupts(|| {
                    let mut l = real.rx_tgt.lock();

                    // Check that buffer isn't already present
                    if l.is_some() {
                        return Some(Err((IoError::Busy, 0)));
                    }
                    let mut count = 0;
                    while let Some(b) = real.receive() {
                        buff[count] = b;
                        count += 1;
                        if count >= buff.len() {
                            return Some(Ok(count));
                        }
                    }

                    *l = Some((buff as *mut [u8], count));
                    None
                }) {
                    // cannot move dbuff must resort to stupid shit like this
                    Some(Ok(count)) => return Ok((dbuff, count)),
                    Some(Err((rc, count))) => return Err((rc, dbuff, count)),
                    _ => {}
                }

                let rc = ReadFut {
                    dispatch: self,
                    phantom_buffer: PhantomData,
                }
                .await;

                match rc {
                    Ok(b) => Ok((dbuff, b.len())),
                    Err((rc, count)) => Err((rc, dbuff, count)),
                }
            } else {
                Err((IoError::NotReady, dbuff, 0))
            }
        }
        .boxed()
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
struct ReadFut<'a, 'b> {
    dispatch: &'a SerialDispatcher,
    phantom_buffer: PhantomData<&'b mut [u8]>,
}

impl<'a, 'b> core::future::Future for ReadFut<'a, 'b> {
    type Output = Result<&'b mut [u8], (IoError, usize)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let real = if self.dispatch.fifo_lock.is_read() {
            self.dispatch
                .inner
                .real
                .upgrade()
                .ok_or((IoError::MediaError, 0))?
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
                Poll::Ready(Ok(unsafe { &mut (&mut (*l.take().unwrap().0))[..len] }))
            } else {
                self.dispatch.inner.stream.register(cx.waker());
                Poll::Pending
            };
        })
    }
}

impl Write<u8> for SerialDispatcher {
    fn write(
        &self,
        _: u64,
        mut dbuff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async {
            if self.fifo_lock.is_write() {
                let buff = &mut *dbuff;
                // Returning here indicates that the driver has closed the controller.
                let Some(real) = self.inner.real.upgrade() else {
                    return Err((IoError::MediaError, dbuff, 0));
                };
                let run = real.run.swap(true, atomic::Ordering::Acquire);
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
                let blen = buff.len();
                Ok((dbuff, blen))
            } else {
                Err((IoError::NotReady, dbuff, 0))
            }
        }
        .boxed()
    }
}

/// This struct is a B-File for [SerialDispatcher].
/// This file contains a Unicode text representing the frame control formatted as a typical UART
///
/// mode string e.g. "115200-8N1".
///
/// Reads return the current mode and will return no more than 10-bytes smaller buffers may return [IoError::EndOfFile].
/// The baud rate is given as an integer, if the baud rate is a non-standard value which is not an
/// integer the actual value will be truncated.
///
/// Writes must are given as a Unicode string which, in order, consists of
/// 1. The baud rate which must be given as an integer inclusive from "115200" and "1"
/// 2. Hyphen separator character
/// 2. A single character ranging from 5 to 8
/// 3. A Parity bit normally "N" (None). Accepted characters are NOMES.
/// 5. Number of stop bits either 1 or 2. (note: 5_2 actually uses 1.5 stop bits)
#[derive(Clone)]
#[kernel_proc_macro::file]
struct FrameCtlBFile {
    dispatch: SerialDispatcher,
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

    fn len(&self) -> IoResult<'_, u64> {
        async { Ok(crate::mem::PAGE_SIZE as u64) }.boxed()
    }
}

impl NormalFile for FrameCtlBFile {
    fn len_chars(&self) -> IoResult<'_, u64> {
        async { Ok(crate::mem::PAGE_SIZE as u64) }.boxed()
    }

    fn file_lock<'a>(
        self: Box<Self>,
    ) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
        async { Err((IoError::NotSupported, self as Box<dyn NormalFile>)) }.boxed()
    }

    unsafe fn unlock_unsafe(&self) -> IoResult<'_, ()> {
        async { Err(IoError::NotSupported) }.boxed()
    }
}

impl Read<u8> for FrameCtlBFile {
    fn read(
        &self,
        _: u64,
        mut dbuff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async {
            let buff = &mut *dbuff;
            let Some(real) = self.dispatch.inner.real.upgrade() else {
                return Err((IoError::MediaError, dbuff, 0));
            };

            let b_rate = 115200f32 / (real.divisor.load(atomic::Ordering::Relaxed) as f32); // use emulated float for conversion to baud-rate
            let data_bits: u8 = real.bits.load(atomic::Ordering::Relaxed).into();
            let parity: char = real.parity.load(atomic::Ordering::Relaxed).into();
            let stop = real.stop.load(atomic::Ordering::Relaxed) as u8 + 1;

            // 10 bytes is the most we write we do this to determine how many bytes we wrote.
            // also moves buff[0..10] into L1D
            let end = buff.len().min(10);
            buff[..end].fill(0);
            let err = write!(buff.writable(), "{b_rate:.0}-{data_bits}{parity}{stop}").is_err();
            let blen = buff.len();
            if err {
                return Err((IoError::EndOfFile, dbuff, blen));
            }

            let len = buff[..10].iter().position(|c| *c == 0).unwrap_or(10);
            Ok((dbuff, len))
        }
        .boxed()
    }
}

impl Write<u8> for FrameCtlBFile {
    fn write(
        &self,
        _: u64,
        mut dbuff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async {
            let buff = &mut *dbuff;
            let s = match core::str::from_utf8(buff) {
                Ok(s) => s,
                Err(_) => return Err((IoError::InvalidData, dbuff, 0)),
            };

            let Some(split) = s.find('-') else {return Err((IoError::InvalidData, dbuff, 0))};
            let (baud_rate, frame) = s.split_at(split);

            let baud_rate: u32 =
                match baud_rate.parse() {
                    Ok(baud) => baud,
                    Err(_) => return Err((IoError::InvalidData, dbuff, 0)),
                };
            if baud_rate > 115200 {
                return Err((IoError::InvalidData, dbuff, 0))
            }

            // performs rounded integer division.
            // there's probably a better way to do this.
            // looks more complicated than it is.
            let divisor: u16 =
                {
                    let clock_rate: u64 = 115200 << 16;
                    let tgt = baud_rate as u64;
                    let div_high = clock_rate / tgt;
                    let div;
                    if div_high & u16::MAX as u64 > (u16::MAX/2) as u64 {
                        div = (div_high >> 16) + 1;
                    } else {
                        div = div_high >> 16;
                    };
                    match div.try_into() {
                        Ok(v) => v,
                        Err(_) => return Err((IoError::InvalidData, dbuff, 0)),
                    }
                };
            if frame.len() != 4 {
                return Err((IoError::InvalidData, dbuff, 0))
            }
            let frame_fmt: [char;3] = {
                let mut f = frame.chars().skip(1);
                let r = [ok_or_lazy!(f.next() => Err((IoError::InvalidData,dbuff,0))),ok_or_lazy!(f.next() => Err((IoError::InvalidData,dbuff,0))),ok_or_lazy!(f.next() => Err((IoError::InvalidData,dbuff,0)))];
                if let Some(_) = f.next() {
                    return Err((IoError::InvalidData, dbuff, 0))
                }
                r
            };
            let data_bits = match frame_fmt[0] {
                '5' => super::DataBits::Five,
                '6' => super::DataBits::Six,
                '7' => super::DataBits::Seven,
                '8' => super::DataBits::Eight,
                _ => return Err((IoError::InvalidData,dbuff,0))
            };

            let parity = match frame_fmt[1].to_ascii_uppercase() {
              // G
                'N' => super::Parity::None,
                'O' => super::Parity::Odd,
                'M' => super::Parity::Mark,
                'E' => super::Parity::Even,
                'S' => super::Parity::Space,
                _ => return Err((IoError::InvalidData,dbuff,0))
            };

            let stop_bits = match frame_fmt[2] {
                '1' => super::StopBits::One,
                '2' => super::StopBits::Two,
                _ => return Err((IoError::InvalidData,dbuff,0))
            };

            let real = ok_or_lazy!(self.dispatch.inner.real.upgrade()  => Err((IoError::MediaError, dbuff,0)));
            real.set_char_mode(data_bits,parity,stop_bits);
            real.set_divisor(divisor);
            let blen = dbuff.len();
            Ok((dbuff, blen))
        }.boxed()
    }
}
