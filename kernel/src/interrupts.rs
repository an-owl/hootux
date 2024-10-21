use crate::gdt;
use crate::interrupts::apic::LOCAL_APIC;
use crate::println;
use log::{error, warn};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

pub mod apic;
pub mod vector_tables;
pub mod buff;

pub const PIC_0_OFFSET: u8 = 32;
pub const PIC_1_OFFSET: u8 = PIC_0_OFFSET + 8;

kernel_proc_macro::interrupt_config!(pub const PUB_VEC_START: u8 = 0x21; fn bind_stubs);

pub static PICS: spin::Mutex<pic8259::ChainedPics> =
    spin::Mutex::new(unsafe { pic8259::ChainedPics::new(PIC_0_OFFSET, PIC_1_OFFSET) });

static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable::new();


pub fn init_exceptions() {
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

    idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
    idt.general_protection_fault.set_handler_fn(except_general_protection);
    idt.segment_not_present.set_handler_fn(except_seg_not_present);
    idt[32].set_handler_fn(crate::mem::tlb::int_shootdown_wrapper);
    idt[33].set_handler_fn(apic_error);
    bind_stubs(&mut idt);
    idt[255].set_handler_fn(spurious);

    // SAFETY: This is the only write to `IDT` and it occurs before multiprocessing is initialized
    unsafe { core::ptr::addr_of_mut!(IDT).write(idt); }
    unsafe { IDT.load() }
}

extern "x86-interrupt" fn except_breakpoint(stack_frame: InterruptStackFrame) {
    println!("Breakpoint, details\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn except_double(stack: InterruptStackFrame, _err: u64) -> ! {
    println!("***DOUBLE FAULT***");
    println!("{:#?}", stack);
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}\n", stack);
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

// SAFETY: References to this must not escape the current CPU
#[thread_local]
static mut RECURSIVE: bool = false;
struct RecursiveLock;
impl RecursiveLock {
    fn new() -> Option<Self> {
        // SAFETY: This is only accessed here and in Self::drop()
        // References are never leaked
        if unsafe { core::mem::replace(&mut RECURSIVE, true) } {
            None
        } else {
            Some(RecursiveLock)
        }
    }
}

impl Drop for RecursiveLock {
    fn drop(&mut self) {
        // SAFETY: This is only accessed here and in Self::new()
        // References are never leaked
        unsafe { RECURSIVE = false }; // fuck of JetBrains this ain't wrong
    }
}


extern "x86-interrupt" fn except_page(sf: InterruptStackFrame, e: PageFaultErrorCode) {
    use x86_64::registers::control::Cr2;

    let r_l = RecursiveLock::new();
    if r_l.is_some() {
        if let Some(fix) = crate::mem::virt_fixup::query_fixup() {
            // SAFETY: This is unsafe because _page_fault_fixup_inner will `core::ptr::read(fix)` and consume it.
            // It is dropped immediately after.
            let fix = core::mem::MaybeUninit::new(fix);
            unsafe {
                core::arch::asm!(
                "xchg rsp,[r12]",
                "call _page_fault_fixup_inner",
                "xchg rsp,[r12]",
                in("r12") &sf.stack_pointer,
                in("rdi") &fix,
                clobber_abi("C")
            );
            }
            core::mem::forget(fix);
            return;
        } else {
            if let Ok(()) = crate::mem::frame_attribute_table::ATTRIBUTE_TABLE_HEAD.fixup(Cr2::read().unwrap()) {
                return;
            }
        }
    }


    println!("*EXCEPTION: PAGE FAULT*\n");

    let fault_addr = Cr2::read().unwrap();
    println!("At address {:?}", Cr2::read());

    if (fault_addr > sf.stack_pointer) && fault_addr < (sf.stack_pointer + 4096u64) {
        println!("Likely Stack overflow")
    }
    if r_l.is_none() {
        println!("Recursive page fault");
    }

    println!("Error code {:?}\n", e);
    println!("{:#?}", sf);
    panic!("page fault");
}

/// This function consumes `fix` and the caller must call [core::mem::forget] on it immediately
/// after calling it.
#[no_mangle]
unsafe extern "C" fn _page_fault_fixup_inner(fix: *mut crate::mem::virt_fixup::CachedFixup) {
    unsafe { fix.read().fixup(); }
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

extern "x86-interrupt" fn except_seg_not_present(sf: InterruptStackFrame, e: u64) {
    panic!("**SEGMENT NOT PRESENT**\n{sf:#?}\n{e:#x}")
}

#[test_case]
fn test_breakpoint() {
    init_exceptions();

    x86_64::instructions::interrupts::int3();
}

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum InterruptIndex {
    Keyboard,
    TlbShootdown, // 0x21
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    Generic(u8),
}

impl From<u8> for InterruptIndex {
    fn from(value: u8) -> Self {
        Self::Generic(value)
    }
}

impl InterruptIndex {
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            Self::Keyboard => PIC_0_OFFSET + 1,
            Self::TlbShootdown => 0x20,
            Self::Generic(n) => n,
        }
    }

    fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }

    /// Binds the GSI to the IRQ represented by self.
    /// This will configure the interrupt using `config`.
    /// The vector field of `config` will be replaced with the real vector allocated to this IRQ
    pub fn get_gsi(&self, gsi: u8) -> apic::ioapic::GlobalSystemInterrupt {
        apic::ioapic::GlobalSystemInterrupt::new(gsi, self.as_u8())
    }

    /// Returns a [apic::ioapic::GlobalSystemInterrupt] for a legacy ISA interrupt.
    /// If a [crate::system::sysfs::systemctl::InterruptOverride] is returned then this fn will
    /// attempt to set the `polarity` and `trigger_mode` if they are defined otherwise they must  be
    /// configured by the caller.
    /// It is UB to modify `polarity` and `trigger_mode` if they are defined by the override struct
    pub fn get_isa(
        &self,
        isa_irq: u8,
    ) -> (
        apic::ioapic::GlobalSystemInterrupt,
        Option<crate::system::sysfs::systemctl::InterruptOverride>,
    ) {
        if let Some(v) = crate::system::sysfs::get_sysfs()
            .systemctl
            .ioapic
            .lookup_override(isa_irq)
        {
            let mut gsi = apic::ioapic::GlobalSystemInterrupt::new(
                v.global_system_interrupt as u8,
                self.as_u8(),
            );
            if let Some(p) = v.polarity {
                gsi.polarity = p;
            }
            if let Some(m) = v.trigger_mode {
                gsi.trigger_mode = m;
            }
            (gsi, Some(v))
        } else {
            (self.get_gsi(isa_irq), None)
        }
    }

    /// Sets the interrupt handler in the interrupt handler registry
    ///
    /// The caller must ensure that hte registered handler correctly handles any interrupts raised
    #[track_caller]
    pub fn set(&self, handler: vector_tables::InterruptHandleContainer) {
        assert!(handler.callable().is_some(), "Tried to set invalid handler");
        vector_tables::IHR.set(self.as_u8(), handler).unwrap() // ?
    }
}


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
        .reserve_contiguous(req_priority.max(PUB_VEC_START), count) // vec[0..32] is reserved for exceptions
        .map_err(|n| n)?;

    ihr.free();

    Ok(n)
}

/// Reserves a single irq without locking the IHR
pub fn reserve_single(req_priority: u8) -> Option<u8> {
    vector_tables::IHR.reserve_contiguous(req_priority.max(PUB_VEC_START), 1).ok()
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

pub fn load_idt() {
    unsafe { IDT.load(); }
}