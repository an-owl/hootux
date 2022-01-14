#![no_std]
#![cfg_attr(test, no_main)]
#![feature(const_mut_refs)]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]
//for interrupts.rs
#![feature(abi_x86_interrupt)]

pub mod gdt;
pub mod interrupts;
pub mod serial;
pub mod vga_text;
pub mod mem;

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
    unsafe { interrupts::PICS.lock().initialize() }
    x86_64::instructions::interrupts::enable();
}

#[inline]
pub fn stop() -> !{
    loop{
        x86_64::instructions::hlt()
    }
}

pub fn test_panic(info: &core::panic::PanicInfo) -> ! {
    serial_println!("[FAILED]");
    serial_println!("Error: {}", info);
    exit_qemu(QemuExitCode::Failed);
    loop {}
}

#[cfg(test)]
use bootloader::{entry_point,BootInfo};

#[cfg(test)]
entry_point!(kernel_test_main);

#[cfg(test)]
#[no_mangle]
fn kernel_test_main(_b: &'static BootInfo) -> ! {
    init();
    test_main();
    loop {x86_64::instructions::hlt()}
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
