
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
pub trait WritableBuffer {
    fn writable(self) -> impl core::fmt::Write;
}

impl<'a> WritableBuffer for &'a mut [u8] {
    fn writable(self) -> impl core::fmt::Write {
        WritableByteArray{
            arr: self,
            cursor: 0,
        }
    }
}

struct WritableByteArray<'a> {
    arr: &'a mut [u8],
    cursor: usize,
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