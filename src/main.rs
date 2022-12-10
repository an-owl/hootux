#![no_std]
#![no_main]
#![feature(const_mut_refs)]
#![feature(custom_test_frameworks)]
#![feature(allocator_api)]
#![test_runner(hootux::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::boxed::Box;
use bootloader::entry_point;
use hootux::exit_qemu;
use hootux::graphics::basic_output::BasicTTY;
use hootux::interrupts::apic::Apic;
use hootux::task::keyboard;
use hootux::task::{executor, Task};
use hootux::time::kernel_init_timer;
use hootux::*;
use log::debug;
use x86_64::VirtAddr;

entry_point!(kernel_main);
#[no_mangle]
fn kernel_main(b: &'static mut bootloader::BootInfo) -> ! {
    //initialize system

    serial_println!("Kernel start");

    if let Some(g) = b.framebuffer.as_mut() {
        g.buffer_mut().fill_with(|| 0xff)
    }

    let mapper;
    unsafe {
        mapper = mem::init(VirtAddr::new(
            b.physical_memory_offset.into_option().unwrap(),
        ));
        let f_alloc = mem::BootInfoFrameAllocator::init(&b.memory_regions);

        init(); // todo break apart

        mem::set_sys_frame_alloc(f_alloc);

        mem::set_sys_mem_tree_no_cr3(mapper);

        allocator::init_comb_heap(0x4444_4000_0000);
    }

    if let bootloader::boot_info::Optional::Some(tls) = b.tls_template {
        // SAFETY: this is safe because the data given is correct
        unsafe {
            mem::thread_local_storage::init_tls(
                tls.start_addr as usize as *const u8,
                tls.file_size as usize,
                tls.mem_size as usize,
            )
        }
    }

    interrupts::apic::load_apic();
    // SAFETY: prob safe but i dont want to think rn
    unsafe { interrupts::apic::get_apic().set_enable(true) }

    //initialize graphics
    if let Some(buff) = b.framebuffer.as_mut() {
        let mut g = graphics::GraphicalFrame { buff };
        g.clear();
        let tty = BasicTTY::new(g);

        unsafe {
            graphics::basic_output::WRITER = spin::Mutex::new(Some(tty));
        }
    };

    unsafe {
        let t = acpi::AcpiTables::from_rsdp(
            system::acpi::AcpiGrabber,
            *b.rsdp_addr.as_mut().unwrap() as usize,
        )
        .unwrap();
        let fadt = acpi::PlatformInfo::new(&t).unwrap();
        let pmtimer = fadt.pm_timer.expect("No PmTimer found");
        let timer = Box::new(time::acpi_pm_timer::AcpiTimer::locate(pmtimer));
        kernel_init_timer(timer);
    }
    // temporary, until thread local segment is set up
    interrupts::apic::cal_and_run(0x20000, 50);

    init_logger();

    say_hi();

    debug!("Successfully initialized");

    #[cfg(test)]
    test_main();

    let mut executor = executor::Executor::new();
    executor.spawn(Task::new(keyboard::print_key()));
    executor.run();
}

fn say_hi() {
    println!("Starting Hootux");
    println!(r#" |   |   \---/   "#);
    println!(r#"\    |  {{\OvO/}}  "#);
    println!(r#"\    |  '/_o_\'  "#);
    println!(r#" | _  >===;=;===="#);
    println!(r#" |( )/ "#);
    println!(r#" | " | "#);
    println!(r#" /    \"#);
}

#[cfg(not(test))]
#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    unsafe {
        panic_unlock!();
    }
    serial_println!("KERNEL PANIC\nInfo: {}", info);
    log::error!("KERNEL PANIC\nInfo: {}", info);

    stop()
}

#[allow(dead_code)]
fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run()
    }
    exit_qemu(QemuExitCode::Success);
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    test_panic(info)
}
