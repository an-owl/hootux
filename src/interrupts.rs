use crate::println;
use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};
use crate::gdt;

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(except_breakpoint);
        unsafe {
            idt.double_fault.set_handler_fn(except_double)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt
    };
}

pub fn init_exceptions() {
    IDT.load()
}

extern "x86-interrupt" fn except_breakpoint(stack_frame: InterruptStackFrame) {
    println!("Breakpoint at: {:#?}", stack_frame);
}

extern "x86-interrupt" fn except_double(stack: InterruptStackFrame, _err: u64) -> ! {
    panic!("EXCEPTION DOUBLE FAULT\n{:#?}",stack);
}

#[test_case]
fn test_breakpoint() {
    init_exceptions();

    x86_64::instructions::interrupts::int3();
}
