use crate::graphics::pixel::Pixel;
use crate::mem;
use alloc::format;
use core::slice::from_raw_parts;
use x86_64::structures::paging::Mapper;

pub mod basic_output;

mod pixel;

pub static KERNEL_FRAMEBUFFER: crate::kernel_structures::KernelStatic<FrameBuffer> =
    crate::kernel_structures::KernelStatic::new();

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

pub trait NewSprite {
    /// Returns the width of `self` in pixels
    fn width(&self) -> usize;

    /// Returns the height of `self` in pixels
    fn height(&self) -> usize;

    /// Returns the pixel format of `self`
    fn format(&self) -> PixelFormat;

    /// Returns a slice containing the pixel data for `self` at the given scan line
    ///
    /// The returned slice will have a len of `self.with()` pixels. The actual value of
    /// `self.scan().len()` will likely not be equal to `self.width()` due to the pixel format
    fn scan(&self, scan_line: usize) -> &[u8];

    fn scan_mut(&mut self, scan_line: usize) -> &mut [u8];

    /// `other` will be copied into self at the given coordinates within self.
    fn draw_into_self<T: NewSprite>(&mut self, other: &T, x: usize, y: usize) {
        let do_width = {
            // width in px
            let base = self.width();
            let chk = (x + other.width()).checked_sub(base);
            let ret = chk.map_or(other.width(), |n| other.width().checked_sub(n).unwrap_or(0));
            ret
        };

        let do_height = {
            let base = self.height();
            let chk = (y + other.height()).checked_sub(base);
            let ret = chk.map_or(other.height(), |n| {
                other.height().checked_sub(n).unwrap_or(0)
            });
            ret
        };

        for (i, line) in (y..(do_height + y)).enumerate() {
            // This looks big and complicated but it doesnt physically do much
            let range = ..do_width * other.format().bytes_per_pixel() as usize;
            let scan = other.scan(i);
            let origin_raw = &scan[range];

            let self_fmt = self.format();

            reformat_px_buff(
                origin_raw,
                &mut self.scan_mut(line)[x * self_fmt.bytes_per_pixel() as usize
                    ..(x + do_width) * self_fmt.bytes_per_pixel() as usize],
                //[start_byte..start_byte + do_width * self.format().bytes_per_pixel() as usize],
                other.format(),
                self_fmt,
            )
            .expect(&format!(
                "Failed to convert between Pixel types {:?} -> {:?}",
                other.format(),
                self.format()
            ));
        }
    }
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

    const fn from_bootloader_info(
        layout: bootloader_api::info::PixelFormat,
        len: usize,
    ) -> Option<Self> {
        use bootloader_api::info::PixelFormat as FarFormat;
        match (layout, len) {
            (FarFormat::Bgr, 4) => Some(Self::Bgr4Byte),
            (FarFormat::Bgr, 3) => Some(Self::Bgr3Byte),
            (FarFormat::U8, 1) => Some(Self::Grey1Byte),
            _ => None,
        }
    }
}

/// Copies `origin` into `dst` while converting the pixel format from `org_fmt` into `new_fmt`.
/// Err(()) will be returned if `new_fmt` is Greyscale and `org_fmt` is **not** Greyscale. Info on
/// why is in source.
///
/// # Panics
///
/// This fn will panic if origin and dst do not have the same length in pixels.
///
/// This fn cannot currently do most conversions,Greyscale can be converted into BGR formats and
/// that's it, all other formats are planned.
fn reformat_px_buff(
    origin: &[u8],
    dst: &mut [u8],
    org_fmt: PixelFormat,
    new_fmt: PixelFormat,
) -> Result<(), ()> {
    assert_eq!(
        origin.len() / org_fmt.bytes_per_pixel() as usize,
        dst.len() / new_fmt.bytes_per_pixel() as usize
    );
    match (org_fmt, new_fmt) {
        (o, d) if o == d => {
            dst.copy_from_slice(&origin[..dst.len()]);
        }

        (PixelFormat::Grey1Byte, PixelFormat::Bgr3Byte) => {
            for (i, p) in unsafe {
                core::mem::transmute::<&[u8], &[pixel::PixGrey1Byte]>(origin)
                    .iter()
                    .enumerate()
            } {
                let bs_start = i * PixelFormat::Bgr3Byte.bytes_per_pixel() as usize;
                let px: pixel::PixBgr3Byte = p.clone().into();
                unsafe {
                    px.copy_to_buff(
                        &mut dst
                            [bs_start..bs_start + PixelFormat::Bgr3Byte.bytes_per_pixel() as usize],
                    )
                }
            }
        }

        (PixelFormat::Grey1Byte, PixelFormat::Bgr4Byte) => {
            for (i, p) in unsafe {
                core::mem::transmute::<&[u8], &[pixel::PixGrey1Byte]>(origin)
                    .iter()
                    .enumerate()
            } {
                let bs_start = i * PixelFormat::Bgr4Byte.bytes_per_pixel() as usize;
                let px: pixel::PixBgr4Byte = p.clone().into();
                unsafe {
                    px.copy_to_buff(
                        &mut dst
                            [bs_start..bs_start + PixelFormat::Bgr4Byte.bytes_per_pixel() as usize],
                    )
                }
            }
        }

        (PixelFormat::Bgr3Byte, PixelFormat::Bgr4Byte) => {
            let arr = unsafe {
                from_raw_parts(
                    origin.as_ptr().cast::<pixel::PixBgr3Byte>(),
                    origin.len() / PixelFormat::Bgr3Byte.bytes_per_pixel() as usize,
                )
            };
            for (i, p) in arr.iter().enumerate() {
                let bs_start = i * PixelFormat::Bgr4Byte.bytes_per_pixel() as usize;
                let px: pixel::PixBgr4Byte = p.clone().into();
                unsafe {
                    px.copy_to_buff(
                        &mut dst
                            [bs_start..bs_start + PixelFormat::Bgr4Byte.bytes_per_pixel() as usize],
                    )
                }
            }
        }

        // grayscale isn't just (r+g+b)/3 it needs floating point to do properly/easily
        // see https://goodcalculators.com/rgb-to-grayscale-conversion-calculator/
        (_, PixelFormat::Grey1Byte) => return Err(()),

        (_, _) => todo!(),
    }
    return Ok(());
}

/*
fn reformat_px<D: pixel::Pixel, S: pixel::Pixel>(dst: &mut [D], src: &[S]) -> Result<(), ()> {
    assert_eq!(src.len(), dst.len()); // check if size in px is equal

    match (D::layout(), S::layout()) {
        (d, s) if d == s => dst.copy_from_slice(unsafe { &*(src as *const [S] as *const [D]) }),

        (_, PixelFormat::Grey1Byte) => panic!("Cannot convert colour formats into Greyscale"),

        (_, _) => {
            for (dst, src) in core::iter::zip(dst, src) {
                let fmt = D::from_pix_data(pix.pix_data());
                *dst = *fmt;
            }
        }
    }

    Ok(())
}
 */

pub struct FrameBuffer {
    height: usize,
    width: usize,
    stride: usize,
    format: PixelFormat,
    data: &'static mut [u8],
}

impl FrameBuffer {
    // TODO address buff as pixels
    /// Scrolls currently displayed frame upward by `l` scan lines
    pub fn scroll_up(&mut self, l: usize) {
        let scroll_px = l * self.stride * self.format.bytes_per_pixel() as usize;

        self.data.copy_within(scroll_px.., 0); // copies from `scroll_px..` to 0 (scroll_px becomes index 0)
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

    #[inline]
    pub fn clear(&mut self) {
        self.data.fill_with(|| 0);
    }

    pub fn info(&self) -> (usize, usize, usize, PixelFormat) {
        (self.height, self.width, self.stride, self.format)
    }
}

impl From<bootloader_api::info::FrameBuffer> for FrameBuffer {
    fn from(mut value: bootloader_api::info::FrameBuffer) -> Self {
        let info = value.info();

        {
            let addr = value.buffer().as_ptr() as usize as u64;
            let start_page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(x86_64::VirtAddr::new(addr));
            let end_page = x86_64::structures::paging::Page::containing_address(
                x86_64::VirtAddr::new(addr + value.buffer().len() as u64),
            );
            let range = x86_64::structures::paging::page::PageRangeInclusive {
                start: start_page,
                end: end_page,
            };
            let mut mapper = mem::SYS_MAPPER.get();
            for page in range {
                use x86_64::structures::paging::PageTableFlags;
                unsafe {
                    mapper
                        .update_flags(
                            page,
                            PageTableFlags::PRESENT
                                | PageTableFlags::WRITABLE
                                | PageTableFlags::NO_CACHE
                                | PageTableFlags::WRITE_THROUGH,
                        )
                        .unwrap()
                        .flush();
                }
            }
        }

        Self {
            height: info.height,
            width: info.width,
            stride: info.stride,
            format: PixelFormat::from_bootloader_info(info.pixel_format, info.bytes_per_pixel)
                .expect("Unsupported PixelFormat"),
            // SAFETY: this is safe because the buffer is points to valid accessible unaliased memory.
            data: unsafe { &mut *(value.buffer_mut() as *mut [u8]) },
        }
    }
}

impl NewSprite for FrameBuffer {
    #[inline]
    fn width(&self) -> usize {
        self.width
    }

    #[inline]
    fn height(&self) -> usize {
        self.height
    }

    #[inline]
    fn format(&self) -> PixelFormat {
        self.format
    }

    #[inline]
    fn scan(&self, scan_line: usize) -> &[u8] {
        let start = scan_line * self.stride;
        &self.data[start..(start + self.width)]
    }

    fn scan_mut(&mut self, scan_line: usize) -> &mut [u8] {
        let start = scan_line * self.stride;
        &mut self.data[start..(start + self.width)]
    }

    fn draw_into_self<T: NewSprite>(&mut self, other: &T, x: usize, y: usize) {
        let do_width = {
            // width in px
            let base = self.width;
            let chk = (x + other.width()).checked_sub(base);
            let ret = chk.map_or(other.width(), |n| other.width().checked_sub(n).unwrap_or(0));
            ret
        };

        let do_height = {
            let base = self.height;
            let chk = (y + other.height()).checked_sub(base);
            let ret = chk.map_or(other.height(), |n| {
                other.height().checked_sub(n).unwrap_or(0)
            });
            ret
        };

        for (i, line) in (y..(do_height + y)).enumerate() {
            let start_byte = (self.stride * line + x) * self.format.bytes_per_pixel() as usize;

            // This looks big and complicated but it doesnt physically do much
            let range = ..do_width * other.format().bytes_per_pixel() as usize;
            let scan = other.scan(i);
            let origin_raw = &scan[range];

            reformat_px_buff(
                origin_raw,
                &mut self.data
                    [start_byte..start_byte + do_width * self.format.bytes_per_pixel() as usize],
                other.format(),
                self.format,
            )
            .expect(&format!(
                "Failed to convert between Pixel types {:?} -> {:?}",
                other.format(),
                self.format
            ));
        }
    }
}
