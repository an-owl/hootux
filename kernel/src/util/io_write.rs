/// Extension trait to allow implementing [core;:fmt::Write]
///
/// # Example
///
/// For types where this trait is implemented
///
///``` ignore
/// fn format_bytes(bytes: &mut [u8]) {
///     use crate::util::WritableBuffer;
///
///     let _ = write!(bytes.writable(), "{}", "Hello, World!")
/// }
pub trait ToWritableBuffer {
    fn writable(self) -> impl WriteableBuffer;
}

pub trait WriteableBuffer: core::fmt::Write {
    fn cursor(&self) -> usize;
}

impl<'a> ToWritableBuffer for &'a mut [u8] {
    fn writable(self) -> impl WriteableBuffer {
        WritableByteArray {
            arr: self,
            cursor: 0,
        }
    }
}

struct WritableByteArray<'a> {
    arr: &'a mut [u8],
    pub cursor: usize,
}

impl<'a> WriteableBuffer for WritableByteArray<'a> {
    fn cursor(&self) -> usize {
        self.cursor
    }
}

impl core::fmt::Write for WritableByteArray<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let len = self.arr[self.cursor..].len().min(s.as_bytes().len());
        self.arr[self.cursor..len + self.cursor].copy_from_slice(&s.as_bytes()[..len]);
        self.cursor += len;
        if len > self.arr.len() {
            Err(core::fmt::Error)
        } else {
            Ok(())
        }
    }
}
