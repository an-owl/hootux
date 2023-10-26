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
#![feature(extract_if)]
#![feature(linked_list_cursors)]
#![feature(core_intrinsics)]
#![feature(box_into_inner)]

extern crate alloc;
pub use mem::allocator::alloc_interface;

mod device_check;
pub mod gdt;
pub mod graphics;
pub mod interrupts;
mod kernel_structures;
mod logger;
pub mod mem;
pub mod runlevel;
pub mod serial;
pub mod system;
pub mod task;
pub mod time;

#[thread_local]
static WHO_AM_I: kernel_structures::UnlockedStatic<u32> = kernel_structures::UnlockedStatic::new();

/// Return the ApicId given by the APIC where available or via CPUID.
fn who_am_i() -> u32 {
    if runlevel::runlevel() != runlevel::Runlevel::PreInit {
        if let Some(i) = WHO_AM_I.try_get() {
            return i.clone();
        }
    }

    let cpuid = raw_cpuid::CpuId::new();
    if let Some(_) = cpuid.get_extended_topology_info_v2() {
        raw_cpuid::cpuid!(0x1f).edx
    } else if let Some(_) = cpuid.get_extended_topology_info() {
        raw_cpuid::cpuid!(0xb).edx
    } else {
        return cpuid.get_feature_info().unwrap().initial_local_apic_id() as u32;
    }
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

pub fn p_pat() {
    use x86_msr::Msr;
    log::debug!("{:?}", unsafe { x86_msr::architecture::Pat::read() })
}

pub fn init() {
    serial_println!("Called init");
    gdt::init();
    interrupts::init_exceptions();
    //unsafe { interrupts::PICS.lock().initialize() }
    unsafe { interrupts::PICS.lock().disable() }
    x86_64::instructions::interrupts::enable();
    mem::write_combining::init_wc();
    serial_println!("Exited init")
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
