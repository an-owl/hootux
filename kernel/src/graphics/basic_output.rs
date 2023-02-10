use super::*;
use crate::serial_println;
use core::fmt;
use core::fmt::Write;
use x86_64::instructions::interrupts::without_interrupts;

const FONT_SIZE: noto_sans_mono_bitmap::RasterHeight = noto_sans_mono_bitmap::RasterHeight::Size16;
const FONT_WEIGHT: noto_sans_mono_bitmap::FontWeight = noto_sans_mono_bitmap::FontWeight::Regular;

type LockedFb<'a> = crate::kernel_structures::static_protected::Ref<'a, FrameBuffer>;

//TODO add scheduled write from buffer
pub static WRITER: spin::Mutex<Option<BasicTTY>> = spin::Mutex::new(None);

//assume framebuffer is always `Some`
pub struct BasicTTY {
    framebuffer: &'static crate::kernel_structures::KernelStatic<FrameBuffer>,

    cursor_x: usize,
    cursor_y: usize,

    cursor_x_max: usize,
    cursor_y_max: usize,

    char_width: usize,
    char_height: usize,
}

impl BasicTTY {
    /// create new BasicTTY
    pub fn new(buff: &'static crate::kernel_structures::KernelStatic<FrameBuffer>) -> Self {
        let char_width = noto_sans_mono_bitmap::get_raster_width(FONT_WEIGHT, FONT_SIZE);
        let char_height = FONT_SIZE.val();

        let lock = buff.get();

        let cursor_x_max = lock.width / char_width;
        let cursor_y_max = (lock.height / char_height) - 1;

        Self {
            framebuffer: buff,
            cursor_x: 0,
            cursor_y: 0,
            cursor_x_max,
            cursor_y_max,
            char_width,
            char_height,
        }
    }

    /// Prints a single character to the screen
    pub fn print_char(&mut self, c: char) {
        self.print_char_inner(c, &mut self.framebuffer.get());
    }

    #[inline]
    fn print_char_inner(&mut self, c: char, fb: &mut LockedFb) {
        match c {
            '\n' => {
                self.newline_inner(fb);
                self.carriage_return();
            }
            '\r' => self.carriage_return(),
            c => {
                if let Some(bitmap) = CharRaster::new(c) {
                    if self.cursor_x >= self.cursor_x_max {
                        self.newline_inner(fb);
                        self.carriage_return();
                    }

                    let l = &mut *fb;
                    l.draw_into_self(
                        &bitmap,
                        self.cursor_x * self.char_width,
                        self.cursor_y * self.char_height,
                    );
                    self.cursor_x += 1;
                }
            }
        }
    }

    /// Prints a string to the screen
    /// using print char
    pub fn print_str(&mut self, s: &str) {
        for c in s.chars() {
            self.print_char(c)
        }
    }

    /// Advances the line and returns the carriage
    ///
    /// will either scroll text up to create a blank line or move down by one line
    /// depending on the current state
    #[inline]
    pub fn newline(&mut self) {
        self.newline_inner(&mut self.framebuffer.get())
    }

    #[inline]
    fn newline_inner(&mut self, fb: &mut LockedFb) {
        if self.cursor_y + 1 >= self.cursor_y_max {
            let l = self.char_height;
            fb.scroll_up(l);
        } else {
            self.cursor_y += 1;
        }
    }

    /// Returns the cursor the start of the line
    #[inline]
    pub fn carriage_return(&mut self) {
        self.cursor_x = 0
    }

    fn clear(&mut self) {
        self.framebuffer.get().clear();
        self.cursor_x = 0;
        self.cursor_y = 0;
    }
}

struct CharRaster {
    inner: noto_sans_mono_bitmap::RasterizedChar,
}

impl CharRaster {
    fn new(ch: char) -> Option<Self> {
        let raster = noto_sans_mono_bitmap::get_raster(ch, FONT_WEIGHT, FONT_SIZE)?;

        Some(Self { inner: raster })
    }
}

impl NewSprite for CharRaster {
    fn width(&self) -> usize {
        noto_sans_mono_bitmap::get_raster_width(FONT_WEIGHT, FONT_SIZE)
    }

    fn height(&self) -> usize {
        FONT_SIZE.val()
    }

    fn format(&self) -> PixelFormat {
        PixelFormat::Grey1Byte
    }

    fn scan(&self, scan_line: usize) -> &[u8] {
        self.inner.raster()[scan_line]
    }

    fn scan_mut(&mut self, _scan_line: usize) -> &mut [u8] {
        panic!(
            "Tried to read {} as mutable",
            core::any::type_name::<Self>()
        )
    }
}

impl Write for BasicTTY {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.print_str(s);
        Ok(())
    }
}

pub fn _print(args: fmt::Arguments) {
    without_interrupts(|| {
        if let Some(tty) = WRITER.lock().as_mut() {
            tty.write_fmt(args).unwrap() //does not return `err()`
        }
    })
}
pub unsafe fn _panic_print() {
    WRITER.force_unlock()
}

pub fn _clear() {
    without_interrupts(|| {
        if let Some(tty) = WRITER.lock().as_mut() {
            tty.clear()
        }
    })
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::graphics::basic_output::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[macro_export]
macro_rules! clear {
    () => {
        $crate::graphics::basic_output::_clear()
    };
}

#[macro_export]
macro_rules! panic_unlock {
    () => {
        $crate::graphics::basic_output::_panic_print()
    };
}
