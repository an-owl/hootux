#![no_std]
#![no_main]
#![feature(const_mut_refs)]
#![feature(custom_test_frameworks)]
#![test_runner(hootux::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use hootux::*;
use bootloader::entry_point;
use x86_64::VirtAddr;
use hootux::graphics::basic_output::BasicTTY;
use hootux::mem;
use hootux::task::{executor, Task};
use hootux::task::keyboard;
use hootux::exit_qemu;




entry_point!(kernel_main);
#[no_mangle]
fn kernel_main(b: &'static mut bootloader::BootInfo) -> ! {
    //initialize system

    if let Some(g) = b.framebuffer.as_mut(){
        g.buffer_mut().fill_with(||{0xff})
    }

    init();

    //initialize memory things
    let phy_mem_offset = VirtAddr::new(b.physical_memory_offset.into_option().unwrap());

    let mut mapper = unsafe { mem::init(phy_mem_offset)};
    let mut frame_alloc = unsafe { mem::BootInfoFrameAllocator::init(&b.memory_regions) };
    allocator::init_heap(&mut mapper,&mut frame_alloc).expect("heap allocation failed");

    //initialize graphics
    if let Some(buff) = b.framebuffer.as_mut() {
        let mut g = graphics::GraphicalFrame { buff };
        g.clear();
        let tty = BasicTTY::new(g);

        unsafe{
            hootux::graphics::basic_output::WRITER = spin::Mutex::new(Some(tty));
        }


    };

    println!("Starting Hootux");
    println!(r#" |   |   \---/   "#);
    println!(r#"\    |  {{\OvO/}}  "#);
    println!(r#"\    |  '/_o_\'  "#);
    println!(r#" | _  >===;=;===="#);
    println!(r#" |( )/ "#);
    println!(r#" | " | "#);
    println!(r#" /    \"#);

    #[cfg(test)]
    test_main();

    let mut executor = executor::Executor::new();
    executor.spawn(Task::new(keyboard::print_key()));
    executor.run();
}

#[cfg(not(test))]
#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> ! {
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
