#![no_std]
#![no_main]
#![feature(const_mut_refs)]
#![feature(custom_test_frameworks)]
#![test_runner(owl_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use owl_os::*;
use bootloader::entry_point;
use x86_64::structures::paging::Page;
use x86_64::VirtAddr;
use owl_os::mem;


entry_point!(kernel_main);
#[no_mangle]
fn kernel_main(b: &'static bootloader::BootInfo) -> ! {
    //initialize system
    init();
    println!("hello, World!");
    let phy_mem_offset = VirtAddr::new(b.physical_memory_offset);
    let mut mapper = unsafe { mem::init(phy_mem_offset)};
    let mut frame_alloc = unsafe { mem::BootInfoFrameAllocator::init(&b.memory_map) };

    let target_page = Page::containing_address(VirtAddr::new(0xdeadbeef));

    mem::create_example_mapping(target_page,&mut mapper, &mut frame_alloc);

    let page_ptr: *mut u64 = target_page.start_address().as_mut_ptr();
    unsafe  { page_ptr.offset(400).write_volatile(0x_f021_f077_f065_f04e)}


    #[cfg(test)]
    test_main();

    stop()
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
