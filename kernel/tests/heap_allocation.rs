#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(hootux::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::boxed::Box;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use hootux::*;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic(&info)
}

entry_point!(main);
#[no_mangle]
fn main(b: &'static mut BootInfo) -> ! {
    use hootux::allocator;
    use hootux::mem::{self, BootInfoFrameAllocator};
    use x86_64::VirtAddr;

    serial_println!("test");

    init();
    let phy_mem_offset = VirtAddr::new(b.physical_memory_offset.into_option().unwrap());
    let mut mapper = unsafe { mem::init(phy_mem_offset) };
    let mut frame_alloc = unsafe { BootInfoFrameAllocator::init(&b.memory_regions) };
    allocator::init_heap(&mut mapper, &mut frame_alloc).expect("heap allocation failed");

    test_main();

    stop()
}

#[test_case]
fn simple_alloc() {
    let v1 = Box::new(21);
    let v2 = Box::new(99);

    assert_eq!(*v1, 21);
    assert_eq!(*v2, 99);
}

#[test_case]
fn realloc() {
    let n = 1000;
    let mut vec = alloc::vec::Vec::new();
    for i in 0..n {
        vec.push(i);
    }
    assert_eq!(vec.iter().sum::<u64>(), (n - 1) * n / 2);
}

#[test_case]
fn dealloc() {
    for i in 0..core::mem::size_of::<Box<usize>>() / allocator::HEAP_SIZE {
        let x = Box::new(i);
        serial_println!("{}", i);
        assert_eq!(*x, i);
    }
}

#[test_case]
fn many_boxes_long_lived() {
    let long_lived = Box::new(1); // new
    for i in 0..allocator::HEAP_SIZE {
        let x = Box::new(i);
        assert_eq!(*x, i);
    }
    assert_eq!(*long_lived, 1); // new
}
