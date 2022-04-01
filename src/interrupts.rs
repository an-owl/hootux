use crate::gdt;
use crate::println;
use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

//TODO update to use APIC

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
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
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
    println!("At address {:?}",Cr2::read());
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
