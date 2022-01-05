use volatile::Volatile;
use core::fmt;
use spin::Mutex;

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct VgaChar{
    char: u8,
    colour: ColourCode,
}

struct VgaBuffer{
    chars: [[Volatile<VgaChar>; BUFFER_WIDTH]; BUFFER_HEIGHT]
}

pub struct Cursor{
    column: usize,
    colour_code: ColourCode,
    buffer: &'static mut VgaBuffer
}

impl Cursor{

    pub fn write_byte(&mut self, byte: u8){
        match byte{
            b'\n' => self.new_line(),
            byte=> {
                if self.column >= BUFFER_WIDTH{
                    self.new_line()
                }

                let row = BUFFER_HEIGHT - 1;
                let col = self.column;

                let colour = self.colour_code;

                self.buffer.chars[row][col].write(VgaChar {
                    char: byte,
                    colour
                });
                self.column += 1;

            }
        }
    }

    pub fn write_str(&mut self, s:&str) {
        for byte in s.bytes() {
            match byte {
                0x20..=0x7e |  b'\n' => self.write_byte(byte),
                _ => self.write_byte(0xfe),
            }
        }
    }

    fn new_line(&mut self) {
        for row in 1..BUFFER_HEIGHT{
            for col in 0..BUFFER_WIDTH{
                //todo optimize this with copy from slice
                let char = self.buffer.chars[row][col].read();
                self.buffer.chars[row - 1][col].write(char);
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.column = 0
    }
    fn clear_row(&mut self, row: usize){
        let blank = VgaChar {
            char: b' ',
            colour: self.colour_code,
        };

        for col in 0..BUFFER_WIDTH{
            self.buffer.chars[row][col].write(blank);
        }
    }
}

impl fmt::Write for Cursor{
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_str(s);
        Ok(())
    }
}
lazy_static::lazy_static! {
    pub static ref WRITER: Mutex<Cursor> = Mutex::new(Cursor {
        column: 0,
        colour_code: ColourCode::new(Colour::LightGrey,Colour::Black),
        buffer: unsafe {&mut *(0xb8000 as *mut VgaBuffer)}
    });
}

#[allow(dead_code)]
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
#[repr(u8)]
pub enum Colour {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGrey = 7,
    DarkGrey = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColourCode(u8);

impl ColourCode {
    fn new(foreground: Colour, background: Colour) -> Self {
        Self((background as u8) << 4 | (foreground as u8))
    }

}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga_text::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    WRITER.lock().write_fmt(args).unwrap();
}