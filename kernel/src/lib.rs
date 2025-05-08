#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]
//for interrupts.rs
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![feature(int_roundings)]
#![feature(allocator_api)]
#![feature(thread_local)]
#![feature(optimize_attribute)]
#![feature(linked_list_cursors)]
#![feature(box_into_inner)]
#![allow(internal_features)] // I need these
#![feature(link_llvm_intrinsics)]
#![feature(layout_for_ptr)]
#![feature(set_ptr_value)]
#![allow(incomplete_features)]
#![feature(generic_const_exprs)] // https://github.com/rust-lang/rust/issues/134044#issuecomment-2526396815 for why this is here
extern crate alloc;
extern crate self as hootux;
pub use mem::allocator::alloc_interface;

mod device_check;
pub mod fs;
pub mod gdt;
pub mod graphics;
pub mod interrupts;

#[cfg(feature = "kernel-shell")]
pub mod kshell;
pub mod llvm;
mod logger;
pub mod mem;
pub mod mp;
pub mod runlevel;
pub mod serial;
pub mod system;
pub mod task;
pub mod time;
mod util;

pub use util::{ToWritableBuffer, WriteableBuffer};

#[thread_local]
static WHO_AM_I: util::UnlockedStatic<u32> = util::UnlockedStatic::new();

/// Return the ApicId given by the APIC where available or via CPUID.
fn who_am_i() -> u32 {
    if runlevel::runlevel() != runlevel::Runlevel::PreInit {
        if let Some(i) = WHO_AM_I.try_get() {
            return i.clone();
        }
    }

    let cpuid = raw_cpuid::CpuId::new();
    let id = if let Some(_) = cpuid.get_extended_topology_info_v2() {
        raw_cpuid::cpuid!(0x1f).edx
    } else if let Some(_) = cpuid.get_extended_topology_info() {
        raw_cpuid::cpuid!(0xb).edx
    } else {
        return cpuid.get_feature_info().unwrap().initial_local_apic_id() as u32;
    };
    if runlevel::runlevel() != runlevel::Runlevel::PreInit {
        WHO_AM_I.init(id);
        x86_64::instructions::nop();
    }
    id
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
    gdt::init();
    interrupts::init_exceptions();
    //unsafe { interrupts::PICS.lock().initialize() }
    unsafe { interrupts::PICS.lock().disable() }
    x86_64::instructions::interrupts::enable();
    mem::write_combining::init_wc();

    let mut ctl = x86_64::registers::control::Cr0::read();
    ctl.set(x86_64::registers::control::Cr0Flags::WRITE_PROTECT, true);
    // SAFETY: This is safe, we enable the write-protect bit here.
    // enabling this prevents erroneous writes to memory
    unsafe {
        x86_64::registers::control::Cr0::write(ctl);
    }
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
use bootloader::{BootInfo, entry_point};

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
