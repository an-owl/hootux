use crate::gdt;
use crate::print;
use crate::println;
use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

pub const PIC_0_OFFSET: u8 = 32;
pub const PIC_1_OFFSET: u8 = PIC_0_OFFSET + 8;

pub static PICS: spin::Mutex<pic8259::ChainedPics> =
    spin::Mutex::new(unsafe { pic8259::ChainedPics::new(PIC_0_OFFSET, PIC_1_OFFSET) });

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(except_breakpoint);
        unsafe {
            idt.double_fault
                .set_handler_fn(except_double)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
            idt[InterruptIndex::Timer.as_usize()].set_handler_fn(timer_interrupt_handler);
            idt[InterruptIndex::Keyboard.as_usize()].set_handler_fn(keyboard_interrupt_handler);
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
    panic!("EXCEPTION DOUBLE FAULT\n{:#?}\n", stack);
}

extern "x86-interrupt" fn timer_interrupt_handler(_sf: InterruptStackFrame) {
    //print!(".");
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}
extern "x86-interrupt" fn keyboard_interrupt_handler(_sf: InterruptStackFrame) {
    use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};

    lazy_static! {
        static ref KEYBOARD: spin::Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> =
            spin::Mutex::new(Keyboard::new(
                layouts::Us104Key,
                ScancodeSet1,
                HandleControl::Ignore
            ));
    }

    let mut k = KEYBOARD.lock();
    let mut ps_2 = x86_64::instructions::port::Port::new(0x60);

    let scancode: u8 = unsafe { ps_2.read() };
    if let Ok(Some(key_event)) = k.add_byte(scancode) {
        if let Some(key) = k.process_keyevent(key_event) {
            match key {
                DecodedKey::Unicode(c) => print!("{}", c),
                DecodedKey::RawKey(key) => print!("{:?}", key),
            }
        }
    }

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
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
