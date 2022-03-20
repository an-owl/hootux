#![no_std]
#![no_main]

use core::panic::PanicInfo;
use hootux::{exit_qemu, serial_print, serial_println, QemuExitCode};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    should_fail();
    loop {}
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    serial_println!("[PASSED]");
    exit_qemu(QemuExitCode::Success);
    loop {}
}

fn should_fail() {
    serial_print!("Should Panic... \t");
    assert_eq!(0, 1);
}
