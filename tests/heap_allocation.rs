#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(owl_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::boxed::Box;
use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use owl_os::*;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic(&info)
}

entry_point!(main);
#[no_mangle]
fn main(b: &'static BootInfo) -> ! {
    use owl_os::allocator;
    use owl_os::mem::{self, BootInfoFrameAllocator};
    use x86_64::VirtAddr;

    serial_println!("test");

    init();
    let phy_mem_offset = VirtAddr::new(b.physical_memory_offset);
    let mut mapper = unsafe { mem::init(phy_mem_offset)};
    let mut frame_alloc = unsafe { mem::BootInfoFrameAllocator::init(&b.memory_map) };
    allocator::init_heap(&mut mapper,&mut frame_alloc).expect("heap allocation failed");

    test_main();

    stop()
}

#[test_case]
fn simple_alloc(){
    let v1 = Box::new(21);
    let v2 = Box::new(99);

    assert_eq!(*v1,21);
    assert_eq!(*v2,99);
}

#[test_case]
fn realloc(){
    let n = 1000;
    let mut vec = alloc::vec::Vec::new();
    for i in 0..n{
        vec.push(i);
    }
    assert_eq!(vec.iter().sum::<u64>(),(n - 1) * n / 2);
}

#[test_case]
fn dealloc(){
    for i in 0..owl_os::allocator::HEAP_SIZE-1{
        let x = Box::new(i);
        assert_eq!(*x, i);
    }
}