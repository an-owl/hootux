#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(owl_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use owl_os::println;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    test_main();
    loop {}
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    owl_os::test_panic(info);
}

#[test_case]
fn test_println() {
    println!("Test, test, is this thing working")
}
