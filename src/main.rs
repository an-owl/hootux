#![no_std]
#![no_main]
#![feature(const_mut_refs)]



mod vga_text;

#[no_mangle]
pub extern "C" fn _start() -> !{
    use core::fmt::Write;

    println!("hello, World!");




    loop {}
}

#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> !{

    println!("{}", info);

    loop{}
}


