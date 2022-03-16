#![no_std]
#![no_main]
#![feature(const_mut_refs)]
#![feature(custom_test_frameworks)]
#![test_runner(hootux::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use core::fmt::Write;
use hootux::*;
use bootloader::entry_point;
use x86_64::instructions::hlt;
use x86_64::VirtAddr;
use hootux::graphics::{BltPixel, GraphicalFrame, Sprite};
use hootux::graphics::basic_output::BasicTTY;
//use hootux::graphics::vtty::Vtty;
use hootux::mem;
use hootux::task::{executor, Task};
use hootux::task::keyboard;
use hootux::exit_qemu;
use hootux::vga_text::WRITER;




entry_point!(kernel_main);
#[no_mangle]
fn kernel_main(b: &'static mut bootloader::BootInfo) -> ! {
    //initialize system

    if let Some(g) = b.framebuffer.as_mut(){
        g.buffer_mut().fill_with(||{0xff})
    }

    init();

    //doesn't work
    //println!("hello, World!");



    //initialize memory things
    let phy_mem_offset = VirtAddr::new(b.physical_memory_offset.into_option().unwrap());

    let mut mapper = unsafe { mem::init(phy_mem_offset)};
    let mut frame_alloc = unsafe { mem::BootInfoFrameAllocator::init(&b.memory_regions) };
    allocator::init_heap(&mut mapper,&mut frame_alloc).expect("heap allocation failed");


    serial_println!("init graphics");
    //initialize graphics
    if let Some(buff) = b.framebuffer.as_mut() {
        let mut g = graphics::GraphicalFrame { buff };
        g.pix_buff_mut().fill_with(||{BltPixel::new(0,0,0)});
        let mut tty = BasicTTY::new(g);
        tty.print_str("Hello, World!");

        serial_println!("lock t");
        let t = spin::Mutex::new(2);
        let y = t.lock();
        drop(y);
        serial_println!("unlocked t");

        unsafe{
            hootux::graphics::basic_output::WRITER = spin::Mutex::new(Some(tty));
        }


    } else { panic!("graphics not found") };

    print!("cock lmao");

    #[cfg(test)]
    test_main();

    let mut executor = executor::Executor::new();
    executor.spawn(Task::new(thing()));
    executor.spawn(Task::new(keyboard::print_key()));
    executor.run();
}

async fn async_number() -> u32 {
    42069
}

async fn thing() {
    let number = async_number().await;
    println!("async number: {}", number)
}

#[cfg(not(test))]
#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    //println!("KERNEL PANIC\nInfo: {}", info);

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
