#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use core::panic::PanicInfo;
use hootux::{exit_qemu, serial_print, serial_println, QemuExitCode};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    serial_print!("{}::overflow...\t", file!());
    hootux::init();
    init_test_idt();

    overflow();

    panic!("failed to overflow stack")
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    hootux::test_panic(info)
}

#[allow(unconditional_recursion)]
fn overflow() {
    overflow();
    volatile::Volatile::new(0).read();
}

use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

fn init_test_idt() {
    TEST_IDT.load()
}

extern "x86-interrupt" fn test_double_fault(_sf: InterruptStackFrame, _code: u64) -> ! {
    serial_println!("[PASSED]");
    exit_qemu(QemuExitCode::Success);

    loop {}
}

lazy_static! {
    static ref TEST_IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        unsafe {
            idt.double_fault
                .set_handler_fn(test_double_fault)
                .set_stack_index(hootux::gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt
    };
}
