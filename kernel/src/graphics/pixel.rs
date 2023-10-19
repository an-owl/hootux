use crate::graphics::PixelFormat;
use core::ptr::NonNull;

#[repr(C)]
#[derive(Debug, Clone)]
pub struct PixBgr4Byte {
    red: u8,
    green: u8,
    blue: u8,
    _reserved: u8,
}

pub enum GenericPixelData {
    Colour(u8, u8, u8),
    Greyscale(u8),
}

pub trait Pixel: Sized {
    /// Check whether the pixel is a greyscale type
    /// Greyscale currently cannot be constructed from colour values
    fn is_grey(&self) -> bool;

    /// Constructs a [GenericPixelData] from self
    fn pix_data(&self) -> GenericPixelData;

    /// Returns the layout of the pixel which implements [Eq]
    fn layout() -> PixelFormat;

    /// Construct Self from a [GenericPixelData]
    fn from_pix_data(data: GenericPixelData) -> Self;

    /// Converts self into bytes and copies itself into the front of `buff`. Implementations of this fn should
    /// be as fast as possible.
    ///
    /// # Safety
    ///
    /// The caller should ensure that `self` can fit into `buff` this fn makes no safety assurances
    /// although implementations may choose to panic
    unsafe fn copy_to_buff(&self, buff: &mut [u8]);

    /// Converts the buffer into a slice of self.
    ///
    /// # Panics
    ///
    /// This fn will panic if the len of buff is not aligned to the size to Self
    fn from_raw(buff: &[u8]) -> &[Self] {
        assert_eq!(
            buff.len() % core::mem::size_of::<Self>(),
            0,
            "pix buffer not aligned correctly"
        );
        let new_len = buff.len() / core::mem::size_of::<Self>();

        // SAFETY: This is safe because buff is capable of being transmuted into self
        let ptr = buff.as_ptr().cast();
        let slice = unsafe { core::slice::from_raw_parts::<Self>(ptr, new_len) };
        slice
    }
}

impl PixBgr4Byte {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            _reserved: 0,
        }
    }
}

impl Pixel for PixBgr4Byte {
    #[inline]
    fn is_grey(&self) -> bool {
        false
    }

    fn pix_data(&self) -> GenericPixelData {
        GenericPixelData::Colour(self.red, self.green, self.blue)
    }

    fn layout() -> PixelFormat {
        PixelFormat::Bgr4Byte
    }

    fn from_pix_data(data: GenericPixelData) -> Self {
        match data {
            GenericPixelData::Colour(r, g, b) => Self::new(r, g, b),
            GenericPixelData::Greyscale(g) => Self::new(g, g, g),
        }
    }

    #[inline]
    unsafe fn copy_to_buff(&self, buff: &mut [u8]) {
        let t = unsafe { *(self as *const Self as *const [u8; 4]) };
        buff[0..4].copy_from_slice(&t)
    }
}

#[repr(C)]
#[derive(Debug, Clone)]
pub struct PixBgr3Byte {
    red: u8,
    green: u8,
    blue: u8,
}

impl PixBgr3Byte {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

impl Pixel for PixBgr3Byte {
    fn is_grey(&self) -> bool {
        false
    }

    fn pix_data(&self) -> GenericPixelData {
        GenericPixelData::Colour(self.red, self.green, self.blue)
    }

    fn layout() -> PixelFormat {
        PixelFormat::Bgr3Byte
    }

    fn from_pix_data(data: GenericPixelData) -> Self {
        match data {
            GenericPixelData::Colour(r, g, b) => Self::new(r, g, b),
            GenericPixelData::Greyscale(g) => Self::new(g, g, g),
        }
    }

    unsafe fn copy_to_buff(&self, buff: &mut [u8]) {
        let t = unsafe { *(self as *const Self as *const [u8; 3]) };
        buff[..3].copy_from_slice(&t);
    }
}

impl Into<PixBgr4Byte> for PixBgr3Byte {
    fn into(self) -> PixBgr4Byte {
        PixBgr4Byte::from_pix_data(self.pix_data())
    }
}

#[derive(Debug, Clone)]
#[repr(transparent)]
pub struct PixGrey1Byte {
    value: u8,
}

impl PixGrey1Byte {
    pub fn new(value: u8) -> Self {
        Self { value }
    }
}

impl Pixel for PixGrey1Byte {
    fn is_grey(&self) -> bool {
        true
    }

    fn pix_data(&self) -> GenericPixelData {
        GenericPixelData::Greyscale(self.value)
    }

    fn layout() -> PixelFormat {
        PixelFormat::Grey1Byte
    }

    fn from_pix_data(data: GenericPixelData) -> Self {
        match data {
            GenericPixelData::Colour(_, _, _) => panic!("Cannot colour into greyscale"),
            GenericPixelData::Greyscale(g) => Self::new(g),
        }
    }

    /// This implementation is safe and will never panic
    unsafe fn copy_to_buff(&self, buff: &mut [u8]) {
        buff[0] = self.value;
    }
}

impl Into<PixBgr3Byte> for PixGrey1Byte {
    fn into(self) -> PixBgr3Byte {
        PixBgr3Byte::new(self.value, self.value, self.value)
    }
}

impl Into<PixBgr4Byte> for PixGrey1Byte {
    fn into(self) -> PixBgr4Byte {
        PixBgr4Byte::new(self.value, self.value, self.value)
    }
}

pub struct PixBuff<'a> {
    _phantom: core::marker::PhantomData<&'a mut u8>,
    ptr: NonNull<u8>,
    len: usize,
}

impl<'a> PixBuff<'a> {
    pub(super) fn buff<T: Pixel>(&mut self) -> &mut [T] {
        assert_eq!(
            self.len % core::mem::size_of::<T>(),
            0,
            "Misaligned PixBuff len for {}",
            core::any::type_name::<T>()
        );
        let p = self.ptr.cast();
        // SAFETY: This is safe, the pointer points to valid
        unsafe { core::slice::from_raw_parts_mut(p.as_ptr(), self.len / core::mem::size_of::<T>()) }
    }
}

impl<'a> From<&'a mut [u8]> for PixBuff<'a> {
    fn from(value: &'a mut [u8]) -> Self {
        Self {
            _phantom: Default::default(),
            ptr: (&mut value[0]).into(),
            len: value.len(),
        }
    }
}
