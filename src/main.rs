#![no_std]


fn main() {
    println!("Hello, world!");
}


#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo) -> !{
    loop{}
}
