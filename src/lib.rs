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
#![feature(thread_local)] // TODO remove kernel_statics.rs

extern crate alloc;

pub mod gdt;
pub mod interrupts;
pub mod serial;
pub mod graphics;
pub mod mem;
pub mod allocator;
pub mod task;
pub mod acpi_driver;
pub mod time;
pub mod system;
mod kernel_statics;
mod logger;
mod device_check;

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
    unsafe {interrupts::PICS.lock().disable()}
    //x86_64::instructions::interrupts::enable();
}

/// Initializes native memory management. Initializes important thread local and global variables
/// required for proper system management
///
/// this function is unsafe because the caller must ensure that the given arguments are accurate
pub unsafe fn init_mem(phy_mem_offset: u64, mem_map: &'static [MemoryRegion]){

    init();

    let mut mapper = mem::init(VirtAddr::new(phy_mem_offset));
    let mut frame_alloc = mem::BootInfoFrameAllocator::init(mem_map) ;
    allocator::init_heap(&mut mapper, &mut frame_alloc).expect("heap allocation failed");

    let ptt;
    {
        ptt = mem::page_table_tree::PageTableTree::from_offset_page_table(VirtAddr::new(phy_mem_offset), &mut mapper, &mut frame_alloc);
        ptt.set_cr3(&mapper)
    };

    kernel_statics::init_statics(frame_alloc,ptt);
    x86_64::instructions::interrupts::enable();

}

pub fn init_logger(){
    log::set_logger(&kernel_statics::fetch_local().globals().logger).expect("failed to initialize logger");
    log::set_max_level(log::LevelFilter::Trace);
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
    stop()
}

#[cfg(test)]
use bootloader::{entry_point,BootInfo};
use bootloader::boot_info::MemoryRegion;
use x86_64::VirtAddr;

#[cfg(test)]
entry_point!(kernel_test_main);

#[cfg(test)]
#[no_mangle]
fn kernel_test_main(_b: &'static mut BootInfo) -> ! {
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


#[alloc_error_handler]
fn alloc_error_handler(layout: alloc::alloc::Layout) -> !{
    panic!("alloc error {:?}", layout)
}