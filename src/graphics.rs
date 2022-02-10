use alloc::vec::Vec;
use core::mem::size_of;
use core::ops::{Deref, DerefMut};
use core::slice::from_raw_parts_mut;
use bootloader::boot_info::FrameBuffer;

//pub mod vtty;

pub struct GraphicalFrame{
    pub buff: &'static mut FrameBuffer,
}

impl GraphicalFrame{

    /// Render sprite to screen coords
    ///
    /// Silently returns on error
    ///
    /// Does not write into overscan region
    pub fn draw(&mut self, coords: (usize,usize), sprite: Sprite){

        let (x,y) = coords;

        // modified width/height values if far sides of sprite are out of bounds
        let mut mod_w = sprite.width;
        let mut mod_h = sprite.height;

        let width = self.buff.info().horizontal_resolution;
        let height = self.buff.info().vertical_resolution;

        // check sizes
        if (x > width) || (y > height){
            // if x or y is out of bounds return.
            // this is basically the behaviour it would have but with less steps
            return
        }

        // calculate and remove overscan
        if (x + mod_w) > width{
            mod_w = sprite.width - (( x - sprite.width ) - width)
        }
        if (y + mod_h) > height{
            mod_h = sprite.height - ((y - sprite.height) - height)
        }


        let width = self.buff.info().stride;
        // used to calculate index within loop  because of borrow checker
        let index = |coords: (usize,usize)| {
            let mut i = width * coords.1; //get the start of the scan line
        i += coords.0; // add offset of x

        i
        };

        //get pix buff
        let pix_buff = unsafe { self.pix_buff_mut()};


        for scan in 0..mod_h{
            let close_scan_start = index((coords.0,coords.1+scan)); // first byte to write to
            let far_scan_start = sprite.index_of((0,scan)); // first byte to read from

            pix_buff[close_scan_start..close_scan_start + mod_w]
                .copy_from_slice(&sprite.data[far_scan_start..far_scan_start + mod_w])
        }
    }

    /// Converts `mut &[u8]` given by self.buff.buffer() to `&[Bltpixel]`
    ///
    /// This should be safe but it probably isn't
    // remove #allow if this works properly
    #[allow(unused_unsafe)]
    unsafe fn pix_buff_mut(&mut self) -> &mut [BltPixel]{
        let len = self.buff.buffer_mut().len();
        let ptr = self.buff.buffer_mut().as_ptr() as usize;
        // assert framebuffer geometry
        assert_eq!(0, len % size_of::<BltPixel>());


        let base = &mut *(ptr as *mut BltPixel);
        // divide len by `size_of(BltPixel)` or buffer will be overrun
        unsafe {from_raw_parts_mut(base, len/size_of::<BltPixel>())}
    }
}


impl DerefMut for GraphicalFrame{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.buff
    }
}

impl Deref for GraphicalFrame{
    type Target = FrameBuffer;

    fn deref(&self) -> &Self::Target {
        &self.buff
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
    pub fn new_arr_3b(data: &[u8]) -> Result<Vec<BltPixel>,()>{
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
        return Ok(out)
    }

    //todo include colour schemes like greyscale, 1bit with foreground/background colours

}

/// Struct to store graphical simple data
pub struct Sprite{
    pub height: usize,
    pub width: usize,
    data: Vec<BltPixel>,
}

impl Sprite{

    /// Creates a new [Sprite]
    /// using [BltPixel::new_arr_3b()]
    ///
    /// expects 24bit colour data within `data`
    ///
    /// returns `Err(())` if `data.len % 3 != 0`
    pub fn new(height: usize, width: usize, data: &[u8]) -> Result<Self, ()> {
        if let Ok(pixels) = BltPixel::new_arr_3b(data) {
            Ok(Self {
                height,
                width,
                data: pixels
            })

        } else {
            return Err(())
        }
    }

    /// Creates [Sprite] from [BltPixel] in order to use non default Bltpixel constructors
    pub fn from_bltpixel(height: usize, width: usize, data: &[BltPixel]) -> Self{
        Self {
            height,
            width,
            data: Vec::from(data)
        }
    }
}


/// Trait for quickly and cleanly addressing places within an array representing a grid
///
/// Functions within this trait will not check the bounds of the grid
/// and will give erroneous results with erroneous inputs
pub trait AddressableGrid{

    /// Returns width of grid
    fn self_width(&self) -> usize;

    /// Returns array index of given coordinates
    ///
    /// Coordinates are represented as (x,y)
    /// where `(0,0)` is the top left corner
    fn index_of(&self, coords: (usize,usize)) -> usize{
        let mut i = self.self_width() * coords.1; //get the start of the scan line
        i += coords.0; // add offset of x

        i

    }

    fn coords_of(&self, index: usize) -> (usize,usize){
        let y = index / self.self_width();
        let x = index % self.self_width();
        (x,y)
    }
}

impl AddressableGrid for Sprite{
    fn self_width(&self) -> usize {
        self.width
    }
}

impl AddressableGrid for GraphicalFrame{

    /// This may act a bit strange because it uses the stride of the framebuffer
    /// not the width as the stride
    fn self_width(&self) -> usize {
        self.buff.info().stride
    }
}