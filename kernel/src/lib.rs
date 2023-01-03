#![no_std]
#![cfg_attr(test, no_main)]
#![feature(const_mut_refs)]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]
//for interrupts.rs
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![feature(inline_const)]
#![feature(int_roundings)]
#![feature(allocator_api)]
#![feature(thread_local)]
#![feature(optimize_attribute)]

extern crate alloc;

pub mod allocator;
mod device_check;
pub mod gdt;
pub mod graphics;
pub mod interrupts;
mod kernel_structures;
mod logger;
pub mod mem;
pub mod serial;
pub mod system;
pub mod task;
pub mod time;

#[thread_local]
static WHO_AM_I: kernel_structures::UnlockedStatic<u32> = kernel_structures::UnlockedStatic::new();

/// Gets the CPU id given by the apic.
fn who_am_i() -> u32 {
    WHO_AM_I.get().clone()
}

pub trait Testable {
    fn run(&self);
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[PASSED]");
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run()
    }
    exit_qemu(QemuExitCode::Success);
}

pub fn init() {
    gdt::init();
    interrupts::init_exceptions();
    //unsafe { interrupts::PICS.lock().initialize() }
    unsafe { interrupts::PICS.lock().disable() }
    x86_64::instructions::interrupts::enable();
}

pub fn init_logger() {
    log::set_logger(&logger::LOGGER).expect("failed to initialize logger");
    log::set_max_level(log::LevelFilter::Trace);
}

#[inline]
pub fn stop() -> ! {
    loop {
        x86_64::instructions::hlt()
    }
}

pub fn test_panic(info: &core::panic::PanicInfo) -> ! {
    serial_println!("[FAILED]");
    serial_println!("Error: {}", info);
    exit_qemu(QemuExitCode::Failed);
    stop()
}

use bootloader_api::info::MemoryRegion;
#[cfg(test)]
use bootloader::{entry_point, BootInfo};
use x86_64::VirtAddr;

#[cfg(test)]
entry_point!(kernel_test_main);

#[cfg(test)]
#[no_mangle]
fn kernel_test_main(_b: &'static mut BootInfo) -> ! {
    init();
    test_main();
    loop {
        x86_64::instructions::hlt()
    }
}

#[cfg(test)]
#[panic_handler]
pub fn panic(info: &core::panic::PanicInfo) -> ! {
    test_panic(info)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}

#[alloc_error_handler]
fn alloc_error_handler(layout: alloc::alloc::Layout) -> ! {
    panic!("alloc error {:?}", layout)
}
