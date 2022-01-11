#![no_std]
#![no_main]
#![feature(const_mut_refs)]
#![feature(custom_test_frameworks)]
#![test_runner(owl_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use owl_os::*;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    //initialize system
    init();

    println!("hello, World!");

    fn overflow(num: u64) {
        overflow(num + 1)
    }

    overflow(0);

    #[cfg(test)]
    test_main();

    panic!("Almost fell through");
}

#[cfg(not(test))]
#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    println!("{}", info);

    loop {}
}

fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run()
    }
    exit_qemu(QemuExitCode::Success);
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    test_panic(info)
}
