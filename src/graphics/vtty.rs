use alloc::alloc::{alloc, dealloc};
use core::alloc::Layout;
use core::mem::{align_of, size_of};
use core::ops::{Index, IndexMut};
use core::slice::from_raw_parts_mut;
use alloc::vec::Vec;
use core::fmt::{Display, Formatter};
use bootloader::boot_info::FrameBufferInfo;
use noto_sans_mono_bitmap::FontWeight;
use crate::graphics::{BltPixel, GraphicalFrame, Sprite};


const VTTY_HIST_SIZE: usize = 200;
const FONT_SIZE: noto_sans_mono_bitmap::BitmapHeight = noto_sans_mono_bitmap::BitmapHeight::Size14;

//TODO impl write queue
/// Virtual teletype driver for outputting text to the display
pub struct Vtty<'a>{

    text_width : usize,
    text_height: usize,
    char_height: usize,
    char_width : usize,
    lines: Vec<VttyLine<'a>>,
    current_line: usize,
    max_lines: usize,
    cursor: usize,
    charset: CharSet
}

impl<'a> Vtty<'a>{

    /// Creates a new vtty on the heap
    ///
    /// This should only be run once on initialization as it can cause a deadlock
    pub fn new(geom: FrameBufferInfo) -> Self{
        use noto_sans_mono_bitmap::get_bitmap_width;
        let char_width = get_bitmap_width(FontWeight::Regular, FONT_SIZE);
        let char_height = FONT_SIZE.val();
        let text_width = geom.horizontal_resolution / char_width;
        let text_height = geom.vertical_resolution / char_height;
        let mut lines = Vec::with_capacity(VTTY_HIST_SIZE);

        for _ in 0..VTTY_HIST_SIZE{
            lines.push(VttyLine::new(text_width));
        }

        Self{

            text_width,
            text_height,
            char_height,
            char_width,
            lines,
            current_line: 0,
            max_lines: VTTY_HIST_SIZE,
            cursor: 0,
            charset: CharSet::new(),
        }
    }

    /// Writes `buff` to vtty
    pub fn output_to_buff(&'a mut self, buff: &str){
        let mut line = &mut self.lines[self.current_line];

        for c in buff.chars() {
            match c {
                // todo add more escape characters
                '\n' => {
                    //line self.newline()
                    self.current_line = self.add_lines(self.current_line,1);
                    line = &mut self.lines[self.current_line];
                },
                '\r' => self.cursor = 0,
                c => {
                    if self.cursor > line.chars.len() {
                        self.current_line = self.add_lines(self.current_line,1);
                        line = &mut self.lines[self.current_line]
                        //line = self.newline();
                    }
                    line[self.cursor] = c;
                    self.cursor += 1;
                },
            }
        }
    }

    /// Adds a new line
    /// Returns mutable pointer to the new line
    /*
    fn newline(&mut self) -> &'a mut VttyLine {
         self.current_line = self.add_lines(self.current_line,1);
        &mut self.lines[self.current_line]
    }*/

    /// renders Vtty to framebuffer
    // todo use memcpy to to move framebuffer by n lines instead of re-rendering entire screen
    pub async fn render(&mut self, g: &mut GraphicalFrame){
        // get top line
        let top_line;
        let ( num, underflow) = self.current_line.overflowing_sub(self.text_height);

        // does wierd maths
        if underflow{
            top_line = (usize::MAX - num) - self.text_height // get size of underflow subtracts if from text_height
        } else {
            top_line = num;
        }

        let mut line = top_line;
        let mut row = 0;

        while line <= self.current_line{

            line = self.add_lines(line,1);
            let line_ptr = &self.lines[line];

            for i in 0..line_ptr.chars.len(){

                match self.charset.get_char(line_ptr.chars[i]){
                    None => {} // do nothing non-graphical characters shouldn't take space

                    Some(sprite) => {
                        g.draw(
                            (self.char_width * self.cursor, self.char_height * row),
                            sprite
                        );
                        self.cursor += 1;
                    }
                }
            }

            row += 1;

        }


        // render
    }

    /// returns line number of `current` + `n`
    fn add_lines(&self,current: usize, n: usize) -> usize{
        let mut num = current.checked_add(n)
            .expect("Overflow in line adder"); // todo fix this at some point
        num %= self.max_lines;
        num
    }

}


/// Contains text within a single line
///
/// manually calls [alloc::alloc] as the size cannot be determined at compile time
struct VttyLine<'a>{
    //maybe update later to use colours
    chars: &'a mut [char]
}

impl<'a> VttyLine<'a> {

    /// Creates a new VttyLine of size `len`.
    /// `len` should remain constant across all uses.

    /*
    alloc is necessary because the vtty must scale with the screen resolution
    which cannot be known at compile time. Collections cannot be used because
    they may grow out of range of the screen and cause undefined behaviour
     */

    fn new(len: usize) -> Self{
        let arr;

        unsafe {
            let ptr = alloc(
                Layout::from_size_align(
                    len * size_of::<char>(), // ensures that array can fit `len` of chars
                    align_of::<char>() // ensures that array is aligned to `char`
                ).unwrap()) as *mut char; // casts pointer to char
            arr = from_raw_parts_mut(ptr, len);
        }


        Self{
            chars: arr,
        }
    }
}


impl<'a> Drop for VttyLine<'a>{
    fn drop(&mut self) {
    //let p = self.chars.as_ptr().cast::<u8>();
        unsafe {
                dealloc( self.chars.as_mut_ptr().cast::<u8>(), Layout::for_value(self.chars))
        }
    }
}

impl<'a> Index<usize> for VttyLine<'a>{
    type Output = char;

    fn index(&self, index: usize) -> &Self::Output {
        &self.chars[index]
    }
}
impl<'a> IndexMut<usize> for VttyLine<'a>{
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.chars[index]
    }
}

impl<'a> Display for VttyLine<'a>{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut r = core::fmt::Result::Err(Default::default());

        for i in 0..self.chars.len(){
            r = write!(f,"{}",self.chars[i])
        }
        r
    }
}

/// Stores character sprites on the heap for later use
struct CharSet{
    char_sprites: Vec<Sprite>
}

impl CharSet{
    const FIRST_CHAR: u32 = 0x20;
    const LAST_CHAR: u32 = 0x017f;

    /// generates charset sprites
    fn new() -> Self {


        let mut char_sprites = Vec::with_capacity((Self::LAST_CHAR - Self::FIRST_CHAR) as usize);

        /*

         this is a mess of translations but basically
         for each char (as u32) cast it to `char` in first match
         get the bitmap in the second match
         then render the sprite for it
         on None create a blank sprite

        */

        // todo add replacement character

        for i in Self::FIRST_CHAR..Self::LAST_CHAR{
            match char::from_u32(i){
                None => char_sprites.push(Sprite::new(0,0,&[]).unwrap()), //fail

                Some(c) => {
                    match noto_sans_mono_bitmap::get_bitmap(c,FontWeight::Regular,FONT_SIZE) {
                        None => char_sprites.push(Sprite::new(0,0,&[]).unwrap()), //fail
                        Some(bitmap) => {

                            // create vec with appropriate size, will be deallocated immediately
                            let mut compiled: Vec<BltPixel> = Vec::with_capacity(
                                FONT_SIZE.val() * noto_sans_mono_bitmap::get_bitmap_width(
                                    FontWeight::Regular, FONT_SIZE));

                            for scan in bitmap.bitmap(){
                                // bitmap is in a 2d array making it annoying to address
                                // this loop processes each scanline at a time and appends it to a vec
                                compiled.extend_from_slice(&*BltPixel::new_arr_greyscale(scan).unwrap())
                            }


                            Sprite::from_bltpixel(
                                FONT_SIZE.val(),
                                noto_sans_mono_bitmap::get_bitmap_width(FontWeight::Regular,FONT_SIZE),
                                &compiled
                            );

                        }
                    }
                }
            }
        }
        Self{
            char_sprites,
        }
    }

    /// gets Sprite for `char`. returns none if it doesn't exist
    fn get_char(&self, char: char) -> Option<&Sprite> {
        // convert char to index position

        let (char, underflow) = (char as usize).overflowing_sub(Self::FIRST_CHAR as usize);
        if underflow{ return None } // character is non graphical

        // character does not exist
        if self.char_sprites[char].height == 0 || self.char_sprites[char].width == 0{
            return None
        }

        Some(&self.char_sprites[char])
    }
}