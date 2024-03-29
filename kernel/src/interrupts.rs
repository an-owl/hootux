use crate::gdt;
use crate::interrupts::apic::LOCAL_APIC;
use crate::println;
use kernel_interrupts_proc_macro::{gen_interrupt_stubs, set_idt_entries};
use lazy_static::lazy_static;
use log::{error, warn};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

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
    println!("{:#?}", stack);
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

extern "x86-interrupt" fn except_page(sf: InterruptStackFrame, e: PageFaultErrorCode) {
    use x86_64::registers::control::Cr2;
    println!("*EXCEPTION: PAGE FAULT*\n");

    let fault_addr = Cr2::read();
    println!("At address {:?}", Cr2::read());

    if (fault_addr > sf.stack_pointer) && fault_addr < (sf.stack_pointer + 4096u64) {
        println!("--->Likely Stack overflow")
    }

    println!("Error code {:?}\n", e);
    println!("{:#?}", sf);
    panic!("page fault");
}

extern "x86-interrupt" fn except_general_protection(sf: InterruptStackFrame, e: u64) {
    println!("GENERAL PROTECTION FAULT");
    println!("error: {}", e);
    println!("{:#?}", sf);
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
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    Generic(u8),
}

impl From<u8> for InterruptIndex {
    fn from(value: u8) -> Self {
        Self::Generic(value)
    }
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        match self {
            InterruptIndex::Timer => PIC_0_OFFSET,
            InterruptIndex::Keyboard => PIC_0_OFFSET + 1,
            InterruptIndex::Generic(n) => n,
        }
    }

    fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

gen_interrupt_stubs!(34);

/// Attempts to reserve `count` contiguous interrupts. Starting at `req_priority`.
/// If `count` contiguous interrupts cannot be located this fn will return the next highest number
/// of contiguous interrupts. The caller can then make a decision on how to reduce the number of IRQs requested.
/// On success located IRQs will be set to reserved and the first IRQ number will be returned.
/// `req_priority` will select
///
/// # Panics
///
/// This fn will panic if `count == 0`
pub fn reserve_irq(req_priority: u8, count: u8) -> Result<u8, u8> {
    assert!(count > 0);
    let ihr = &vector_tables::IHR;
    ihr.lock();

    let n = ihr
        .reserve_contiguous(req_priority.max(32), count) // vec[0..32] is reserved for exceptions
        .map_err(|n| n)?;

    ihr.free();

    Ok(n)
}

/// Reserves a single irq without locking the IHR
pub fn reserve_single(req_priority: u8) -> Option<u8> {
    vector_tables::IHR.reserve_contiguous(req_priority, 1).ok()
}

/// Registers an interrupt handler to the given IRQ. This function will return `Ok(())` on success
/// and `Err(())` if a handler is already registered to this IRQ.
///
/// # Deadlocks
///
/// This fn will cause a deadlock if an IRQ is requested on the CPU calling this fn.
/// The caller must ensure that no interrupts will be triggered requesting this IRQ. This can be
/// done by clearing the Interrupt Flag or disabling the device requesting the IRQ.
///
/// # Safety
///
/// This fn is unsafe because the caller must ensure that the queue is correctly handled by the task
/// given by `tid`.
pub unsafe fn alloc_irq(
    irq: u8,
    queue: crate::task::InterruptQueue,
    message: u64,
) -> Result<(), ()> {
    let handle = vector_tables::InterruptHandle::new(queue, message);
    vector_tables::IHR.set(
        irq,
        vector_tables::InterruptHandleContainer::Generic(handle),
    )
}

/// Frees the given IRQ.
///
/// # Safety
///
/// The caller must ensure that the IRQ will not be raised.
/// If the IRQ is raised the may cause UB.
pub(crate) unsafe fn free_irq(irq: InterruptIndex) -> Result<(), ()> {
    vector_tables::IHR.free_irq(irq)
}

pub(crate) fn reg_waker(irq: InterruptIndex, waker: &core::task::Waker) -> Result<(), ()> {
    if let vector_tables::InterruptHandleContainer::Generic(g) =
        &*vector_tables::IHR.get(irq.as_u8()).write()
    {
        g.register(waker);
        Ok(())
    } else {
        Err(())
    }
}
