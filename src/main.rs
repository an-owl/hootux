#![no_std]
#![no_main]
#![feature(const_mut_refs)]
#![feature(custom_test_frameworks)]
#![test_runner(hootux::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use hootux::*;
use bootloader::entry_point;
use hootux::graphics::basic_output::BasicTTY;
use hootux::task::{executor, Task};
use hootux::task::keyboard;
use hootux::exit_qemu;
use log::debug;


entry_point!(kernel_main);
#[no_mangle]
fn kernel_main(b: &'static mut bootloader::BootInfo) -> ! {
    //initialize system

    serial_println!("Kernel start");

    if let Some(g) = b.framebuffer.as_mut(){
        g.buffer_mut().fill_with(||{0xff})
    }

    //initialize memory things
    unsafe {
        init_mem(b.physical_memory_offset.into_option().unwrap(), &b.memory_regions)
    }

    //initialize graphics
    if let Some(buff) = b.framebuffer.as_mut() {
        let mut g = graphics::GraphicalFrame { buff };
        g.clear();
        let tty = BasicTTY::new(g);

        unsafe{
            graphics::basic_output::WRITER = spin::Mutex::new(Some(tty));
        }
    };
    init_logger();

    say_hi();

    debug!("Successfully initialized");






    #[cfg(test)]
    test_main();

    let mut executor = executor::Executor::new();
    executor.spawn(Task::new(keyboard::print_key()));
    executor.run();
}

fn say_hi(){
    println!("Starting Hootux");
    println!(r#" |   |   \---/   "#);
    println!(r#"\    |  {{\OvO/}}  "#);
    println!(r#"\    |  '/_o_\'  "#);
    println!(r#" | _  >===;=;===="#);
    println!(r#" |( )/ "#);
    println!(r#" | " | "#);
    println!(r#" /    \"#);
}

#[cfg(not(test))]
#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    unsafe { panic_unlock!(); }
    println!("KERNEL PANIC\nInfo: {}", info);

    stop()
}

#[allow(dead_code)]
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
