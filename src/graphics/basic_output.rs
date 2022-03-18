use core::fmt;
use core::fmt::Write;
use x86_64::instructions::interrupts::without_interrupts;
use super::*;

//TODO add scheduled write from buffer
pub static mut WRITER: spin::Mutex<Option<BasicTTY>> = spin::Mutex::new(None);

//assume framebuffer is always `Some`
pub struct BasicTTY{
    framebuffer: GraphicalFrame,

    cursor_x: usize,
    cursor_y: usize,

    cursor_x_max: usize,
    cursor_y_max: usize,

    char_width: usize,
    char_height: usize,
}

impl BasicTTY{
    const FONT_WEIGHT: noto_sans_mono_bitmap::FontWeight = noto_sans_mono_bitmap::FontWeight::Regular;
    const FONT_SIZE: noto_sans_mono_bitmap::BitmapHeight = noto_sans_mono_bitmap::BitmapHeight::Size14;


    /// create new BasicTTY
    pub fn new(buff: GraphicalFrame) -> Self{



        let char_width = noto_sans_mono_bitmap::get_bitmap_width(Self::FONT_WEIGHT,Self::FONT_SIZE);
        let char_height = Self::FONT_SIZE.val();

        let cursor_x_max = buff.info().horizontal_resolution / char_width;
        let cursor_y_max = buff.info().vertical_resolution / char_height;

        Self{
            framebuffer: buff,
            cursor_x: 0,
            cursor_y: 0,
            cursor_x_max,
            cursor_y_max,
            char_width,
            char_height
        }
    }

    /// Prints a single character to the screen
    pub fn print_char(&mut self, c: char){
        use noto_sans_mono_bitmap::*;
        match c {
            '\n' => {
                self.newline();
                self.carriage_return();
            }
            '\r' => self.carriage_return(),
            c=> {

                if let Some(bitmap) = get_bitmap(
                    c,
                Self::FONT_WEIGHT,
                Self::FONT_SIZE
                ){
                    let mut hold = Vec::with_capacity(bitmap.height() * bitmap.width());
                    for i in bitmap.bitmap(){
                        hold.extend_from_slice(i)
                    }
                    if self.cursor_x >= self.cursor_x_max {
                        self.newline();
                        self.carriage_return();
                    }

                    let char_sprite = Sprite::from_bltpixel(
                        bitmap.height(),
                        bitmap.width(),
                        &*BltPixel::new_arr_greyscale(&hold).unwrap()
                    );

                    self.framebuffer.draw((self.cursor_x * self.char_width, self.cursor_y * self.char_height), &char_sprite);
                    self.cursor_x += 1;
                }
            }
        }
    }

    /// Prints a string to the screen
    /// using print char
    pub fn print_str(&mut self, s: &str){
        for c in s.chars(){
            self.print_char(c)
        }
    }

    /// Advances the line and returns the carriage
    ///
    /// will either scroll text up to create a blank line or move down by one line
    /// depending on the current state
    #[inline]
    pub fn newline(&mut self){
        if self.cursor_y == self.cursor_y_max {
            let l = self.char_height;
            self.framebuffer.scroll_up(l)
        } else {
            self.cursor_y += 1;
        }
    }

    /// Returns the cursor the start of the line
    #[inline]
    pub fn carriage_return(&mut self){
        self.cursor_x = 0
    }
}

impl fmt::Write for BasicTTY{
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.print_str(s);
        Ok(())
    }
}

pub fn _print(args: core::fmt::Arguments){
    without_interrupts(||
        {
            if let Some(tty) = unsafe { WRITER.lock().as_mut() }{
                tty.write_fmt(args).unwrap() //does not return `err()`
            }
        }
    )
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