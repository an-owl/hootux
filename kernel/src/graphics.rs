use crate::graphics::pixel::{PixBgr3Byte, PixBgr4Byte, Pixel};

pub mod basic_output;

pub mod pixel;

pub static KERNEL_FRAMEBUFFER: crate::util::KernelStatic<FrameBuffer> =
    crate::util::KernelStatic::new();

static KERNEL_PIX_FORMAT: atomic::Atomic<PixelFormat> = atomic::Atomic::new(PixelFormat::Bgr4Byte);

/// This fn will set [KERNEL_PIX_FORMAT], which is which is used for selecting the pixel format when
/// creating new sprites. This is used for optimization only but should not be set to a greyscale
/// format as this may cause a panic when reformatting sprites.
///
/// This should not be called at runtime except when initializing or disconnecting modifying
/// framebuffer settings.
pub fn set_default_format(format: PixelFormat) {
    KERNEL_PIX_FORMAT.store(format, atomic::Ordering::Release);
}

/// Returns the kernels default pixel format. This should be used to initialize sprites that are
/// printed to the framebuffer.
pub fn sys_pix_format() -> PixelFormat {
    KERNEL_PIX_FORMAT.load(atomic::Ordering::Relaxed)
}

/// PixelFormat describes the order of bytes and number of bytes in a pixel. This is necessary because pixel formats are not known at compile time and may chane at runtime
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum PixelFormat {
    Bgr4Byte,
    Bgr3Byte,
    Grey1Byte,
}

impl PixelFormat {
    const fn bytes_per_pixel(&self) -> u8 {
        match self {
            PixelFormat::Bgr4Byte => 4,
            PixelFormat::Bgr3Byte => 3,
            PixelFormat::Grey1Byte => 1,
        }
    }
}

pub struct FrameBuffer {
    height: usize,
    width: usize,
    stride: usize,
    format: PixelFormat,
    data: &'static mut [u8],
}

impl FrameBuffer {
    pub fn new(
        width: usize,
        height: usize,
        stride: usize,
        buff: &'static mut [u8],
        format: PixelFormat,
    ) -> Self {
        assert!(
            height * width * (format.bytes_per_pixel() as usize) >= buff.len(),
            "Frame buffer is smaller than resolution requires"
        );
        Self {
            height,
            width,
            stride,
            format,
            data: buff,
        }
    }

    // TODO address buff as pixels
    /// Scrolls currently displayed frame upward by `l` scan lines
    pub fn scroll_up(&mut self, l: usize) {
        let scroll_px = l * self.stride * self.format.bytes_per_pixel() as usize;

        self.data.copy_within(scroll_px..self.last_px(), 0); // copies from `scroll_px..` to 0 (scroll_px becomes index 0)
    }

    #[inline]
    pub fn clear_lines(&mut self, lines: core::ops::Range<usize>) {
        for scan in lines {
            // todo Does this need to be volatile? does it need to write as the specified format?
            let start = scan * self.stride * self.format.bytes_per_pixel() as usize;

            // todo use px type not u8
            self.data[start..start + (self.width * self.format.bytes_per_pixel() as usize)]
                .fill_with(|| 0);
        }
    }

    fn last_px(&self) -> usize {
        self.stride * self.height * self.format.bytes_per_pixel() as usize
    }

    #[inline]
    pub fn clear(&mut self) {
        let l = self.last_px();
        self.data[..l].fill_with(|| 0);
    }

    pub fn info(&self) -> (usize, usize, usize, PixelFormat) {
        (self.height, self.width, self.stride, self.format)
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Returns a mutable reference the requested scan line.
    /// Returns `None` if the requested scan line isn't present.
    fn scan(&mut self, scan: usize) -> Option<&mut [u8]> {
        if self.height >= scan {
            Some(
                &mut self.data[(self.stride * scan) * self.format.bytes_per_pixel() as usize
                    ..((self.stride * scan) + self.width) * self.format.bytes_per_pixel() as usize],
            )
        } else {
            None
        }
    }
}

impl Sprite for FrameBuffer {
    fn width(&self) -> usize {
        self.width
    }
    fn height(&self) -> usize {
        self.height
    }
}

/// Internal pixel conversion into supported pixel formats
fn cvt_px<R, T, P>(raw: R) -> P
where
    T: DrawableSprite<R>,
    P: Pixel + Sized,
{
    let rgb = T::convert_rgb(raw);
    P::from_pix_data(pixel::GenericPixelData::Colour(rgb.0, rgb.1, rgb.2))
}

impl SpriteMut for FrameBuffer {
    fn draw_into_self<R, T: DrawableSprite<R>>(&mut self, other: &T, x: usize, y: usize) {
        for (other_scan, self_scan) in (y..y + other.height()).enumerate() {
            let format = self.format;
            if let Some(buff) = self.scan(self_scan) {
                let mut buff: pixel::PixBuff = buff.into();
                match format {
                    PixelFormat::Bgr4Byte => other.draw_into_scan(
                        other_scan,
                        cvt_px::<R, T, PixBgr4Byte>,
                        &mut buff.buff::<PixBgr4Byte>()[x..],
                    ),
                    PixelFormat::Bgr3Byte => other.draw_into_scan(
                        other_scan,
                        cvt_px::<R, T, PixBgr3Byte>,
                        &mut buff.buff::<PixBgr3Byte>()[x..],
                    ),
                    _ => panic!("Unable to convert between pixel formats"),
                }
            } else {
                break;
            }
        }
    }
}

//TODO: This is a workaround to a bug in bindeps remove all of these when possible, and swap "fontgen_bugfix" for "fontgen"
fn font_map() -> bitmap_fontgen::Font {
    (&font::FONT_MAP).into()
}

mod font {
    use super::Integer;
    use crate::graphics::Sprite;

    pub(super) static FONT_MAP: bitmap_fontgen::ConstFontMap = include!(env!("FONT_MAP_FILE"));

    impl Sprite for bitmap_fontgen::BitMap {
        fn width(&self) -> usize {
            self.size().width as usize
        }

        fn height(&self) -> usize {
            self.size().height as usize
        }
    }

    impl super::DrawableSprite<bool> for bitmap_fontgen::BitMap {
        fn draw_into_scan<T, F>(&self, scan: usize, f: F, buff: &mut [T])
        where
            F: Fn(bool) -> T,
        {
            self.draw_scan(scan.try_into().unwrap(), f, buff)
        }

        fn convert_rgb<T: Integer>(raw: bool) -> (T, T, T) {
            if raw {
                (T::MAX, T::MAX, T::MAX)
            } else {
                (T::MIN, T::MIN, T::MIN)
            }
        }
    }
}

pub trait Sprite {
    /// Returns the width of `self` in pixels
    fn width(&self) -> usize;

    /// Returns the height of `self` in pixels
    fn height(&self) -> usize;
}

trait SpriteMut: Sprite {
    /// `other` will be copied into self at the given coordinates within self.
    fn draw_into_self<R, T: DrawableSprite<R>>(&mut self, other: &T, x: usize, y: usize);
}

/// Trait for drawing sprites. This trait is separate from [Sprite] and [SpriteMut] because some
/// specific implementations may not be compatible with those traits, but may still be converted into a sprite
trait DrawableSprite<R>: Sprite {
    /// Iterates over each pixel in the scan line specified by `scan` calling `f` on it to perform
    /// a conversion from `R` into `T`.
    /// Returns when either the end of the buffer or the end of
    /// `self`'s scan line has been reached.
    fn draw_into_scan<T, F>(&self, scan: usize, f: F, buff: &mut [T])
    where
        F: Fn(R) -> T;

    /// Returns a pointer to a function which can convert `R` into RGB values.
    ///
    /// This is intended to be used as a part of closures to perform the whole conversion,
    /// as opposed to being used to directly perform the conversion.
    /// See implementation of [SpriteMut::draw_into_self] for [FrameBuffer] for an example  
    fn convert_rgb<T: Integer>(raw: R) -> (T, T, T);
}

// todo: move out of here
pub trait Integer:
    core::ops::Add
    + core::ops::Sub
    + core::ops::AddAssign
    + core::ops::SubAssign
    + core::ops::Mul
    + core::ops::MulAssign
    + Sized
{
    const MIN: Self;
    const MAX: Self;
    const BITS: u32;
}

impl Integer for u8 {
    const MIN: Self = Self::MIN;
    const MAX: Self = Self::MAX;
    const BITS: u32 = Self::BITS;
}

impl Integer for u16 {
    const MIN: Self = Self::MIN;
    const MAX: Self = Self::MAX;
    const BITS: u32 = Self::BITS;
}

impl Integer for u32 {
    const MIN: Self = Self::MIN;
    const MAX: Self = Self::MAX;
    const BITS: u32 = Self::BITS;
}

impl Integer for u64 {
    const MIN: Self = Self::MIN;
    const MAX: Self = Self::MAX;
    const BITS: u32 = Self::BITS;
}

impl Integer for i8 {
    const MIN: Self = Self::MIN;
    const MAX: Self = Self::MAX;
    const BITS: u32 = Self::BITS;
}

impl Integer for i16 {
    const MIN: Self = Self::MIN;
    const MAX: Self = Self::MAX;
    const BITS: u32 = Self::BITS;
}

impl Integer for i32 {
    const MIN: Self = Self::MIN;
    const MAX: Self = Self::MAX;
    const BITS: u32 = Self::BITS;
}

impl Integer for i64 {
    const MIN: Self = Self::MIN;
    const MAX: Self = Self::MAX;
    const BITS: u32 = Self::BITS;
}
