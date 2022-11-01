use crate::gdt;
use crate::println;
use lazy_static::lazy_static;
use log::{error, warn};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use kernel_interrupts_proc_macro::{gen_interrupt_stubs, set_idt_entries};
use crate::interrupts::apic::LOCAL_APIC;

pub mod apic;
pub mod vector_tables;

pub const PIC_0_OFFSET: u8 = 32;
pub const PIC_1_OFFSET: u8 = PIC_0_OFFSET + 8;

pub static PICS: spin::Mutex<pic8259::ChainedPics> =
    spin::Mutex::new(unsafe { pic8259::ChainedPics::new(PIC_0_OFFSET, PIC_1_OFFSET) });

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(except_breakpoint);
        // these unsafe blocks set alternate stack addresses ofr interrupts
        unsafe {
            idt.double_fault
                .set_handler_fn(except_double)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        unsafe {
            idt.page_fault
                .set_handler_fn(except_page)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        idt[InterruptIndex::Timer.as_usize()].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_usize()].set_handler_fn(keyboard_interrupt_handler);
        idt.general_protection_fault.set_handler_fn(except_general_protection);
        idt[33].set_handler_fn(apic_error);
        set_idt_entries!(34);
        idt[255].set_handler_fn(spurious);
        idt
    };
}

pub fn init_exceptions() {
    IDT.load()
}

extern "x86-interrupt" fn except_breakpoint(stack_frame: InterruptStackFrame) {
    println!("Breakpoint, details\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn except_double(stack: InterruptStackFrame, _err: u64) -> ! {
    println!("***DOUBLE FAULT***");
    println!("{:#?}",stack);
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}\n", stack);
}

extern "x86-interrupt" fn timer_interrupt_handler(_sf: InterruptStackFrame) {
    // SAFETY: This is safe because declare_eoi does not violate data safety
    unsafe { LOCAL_APIC.force_get_mut().declare_eoi() }

}
extern "x86-interrupt" fn keyboard_interrupt_handler(_sf: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };
    crate::task::keyboard::add_scancode(scancode);

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

extern "x86-interrupt" fn except_page(sf: InterruptStackFrame, e: PageFaultErrorCode){
    use x86_64::registers::control::Cr2;
    println!("*EXCEPTION: PAGE FAULT*\n");

    let fault_addr = Cr2::read();
    println!("At address {:?}",Cr2::read());

    if (fault_addr > sf.stack_pointer) && fault_addr < (sf.stack_pointer + 4096u64){
        println!("--->Likely Stack overflow")
    }

    println!("Error code {:?}\n",e);
    println!("{:#?}",sf);
    panic!("page fault");
}

extern "x86-interrupt" fn except_general_protection(sf: InterruptStackFrame, e: u64){
    println!("GENERAL PROTECTION FAULT");
    println!("error: {}", e);
    println!("{:#?}",sf);
    panic!();
}

extern "x86-interrupt" fn apic_error(_sf: InterruptStackFrame) {
    // SAFETY: This is safe because errors are immediately handled here and should not be
    // accessed outside of this handler
    unsafe {
        let apic = LOCAL_APIC.force_get_mut();
        let mut err = apic.get_err();
        while err.bits() != 0 {
            error!("Apic Error: {err:?}");
            err = apic.get_err()
        }
    }
}

extern "x86-interrupt" fn spurious(sf: InterruptStackFrame) {
    warn!("spurious Interrupt");
    println!("{sf:#?}");
}


#[test_case]
fn test_breakpoint() {
    init_exceptions();

    x86_64::instructions::interrupts::int3();
}

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_0_OFFSET,
    Keyboard,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }

    fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

gen_interrupt_stubs!(34);