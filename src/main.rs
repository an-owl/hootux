#![no_std]
#![no_main]
#![feature(const_mut_refs)]
#![feature(custom_test_frameworks)]
#![test_runner(owl_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;
use owl_os::*;
use bootloader::entry_point;
use x86_64::VirtAddr;
use owl_os::graphics::{BltPixel, GraphicalFrame, Sprite};
use owl_os::graphics::vtty::Vtty;
use owl_os::mem;
use owl_os::task::{executor, Task};
use owl_os::task::keyboard;



entry_point!(kernel_main);
#[no_mangle]
fn kernel_main(b: &'static mut bootloader::BootInfo) -> ! {
    //initialize system
    init();

    //doesn't work
    println!("hello, World!");

    //initialize memory things
    let phy_mem_offset = VirtAddr::new(b.physical_memory_offset.into_option().unwrap());
    let mut mapper = unsafe { mem::init(phy_mem_offset)};
    let mut frame_alloc = unsafe { mem::BootInfoFrameAllocator::init(&b.memory_regions) };
    allocator::init_heap(&mut mapper,&mut frame_alloc).expect("heap allocation failed");

    //initialize graphics
    let mut g: GraphicalFrame = if let Some(buff) = b.framebuffer.as_mut() {
        let mut g = graphics::GraphicalFrame { buff };
        let sprite = Sprite::from_bltpixel(1,1,&[BltPixel::new(0xff,0xff,0xff)]);
        g
    } else { panic!("graphics not found") };

    let mut vtty = Vtty::new(g.info());
    vtty.output_to_buff("Hello, world!");
    vtty.render(&mut g);



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
