use core::mem::size_of;
use core::slice::from_raw_parts_mut;
use bootloader::boot_info::FrameBuffer;
use alloc::vec::Vec;

const FONT_SIZE: noto_sans_mono_bitmap::BitmapHeight = noto_sans_mono_bitmap::BitmapHeight::Size14;
const FONT_WEIGHT: noto_sans_mono_bitmap::FontWeight = noto_sans_mono_bitmap::FontWeight::Regular;

pub static OUTPUT: spin::Mutex<Option<GraphicalText>> = spin::Mutex::new(None);


/// Simple graphics struct
pub struct GraphicalText{
    //graphical metadata
    width: usize,
    height: usize,
    stride: usize,
    buff: &'static mut[BltPixel],

    //cursor location
    cursor_x: usize,
    cursor_y: usize,

    // character geometry
    char_height: usize,
    char_width: usize,

}

impl GraphicalText{

    /// creates new GraphicalText
    fn new(fb: &FrameBuffer) -> Self{
        let info = fb.info();

        let width = info.horizontal_resolution;
        let stride = info.stride;
        let height = info.vertical_resolution;

        let base = ((fb.buffer().as_ptr() as usize) as *mut BltPixel);
        //assert that framebuffer aligns correctly
        assert_eq!(0, fb.buffer().len() % size_of::<BltPixel>());
        let len = fb.buffer().len() / size_of::<BltPixel>();
        let buff;
        unsafe {
             buff =  from_raw_parts_mut(base, len)
        }
        //assert that number of pixels is correct
        assert_eq!(buff.len(), height * stride);

        let char_width = noto_sans_mono_bitmap::get_bitmap_width(FONT_WEIGHT, FONT_SIZE);
        let char_height = FONT_SIZE.val();


        Self{
            width,
            height,
            stride,
            buff,
            cursor_x: 0,
            cursor_y: 0,
            char_height,
            char_width
        }
    }

    fn write_pix(&mut self, colour: BltPixel){
    }
}



/// represents a single pixel
///
/// _reserved byte is saved for copying as 32bit not 3x8bit
#[repr(C)]
#[derive(Copy,Clone)]
pub struct BltPixel {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    _reserved: u8,

}

impl BltPixel{

    /// Creates a new [BltPixel] from colour values
    pub fn new(red: u8, green: u8, blue: u8) -> Self{
        Self{
            red,
            green,
            blue,
            _reserved: 0
        }
    }

    /// Creates an array of \[BltPixel\] from an array of \[u8\]
    /// where each pixel represented ad 3 bits
    ///
    /// Returns err(()) if data is not divisible by 3
    pub fn new_arr_3b(data: &[u8]) -> Result<Vec<Self>,()>{
        //check alignment
        if (data.len() % 3) != 0 { return Err(()) }

        let mut out = Vec::with_capacity(data.len()/3);

        for i in 0..data.len()/3{
            let base = i * 3;
            out.push(Self::new(
                data[base+0],
                data[base+1],
                data[base+2],
            ))
        }
        Ok(out)
    }

    /// Creates new \[Bltpixel\] using a greyscale format
    ///
    /// Bytes are interpreted as a scale between high and low where a byte set to `0xff` == high
    /// i.e. to create a greyscale image low == Black and high == White. the value of &data will be the intensity
    pub fn new_arr_greyscale(data: &[u8]) -> Result<Vec<Self>,()> {

        let mut out = Vec::with_capacity(data.len()/size_of::<BltPixel>());
        for b in data{
            out.push(BltPixel::new(*b,*b,*b));
        }
        Ok(out)
    }

    //todo include colour schemes like greyscale, 1bit with foreground/background colours

}

