#![no_std]
#![no_main]
#![feature(const_mut_refs)]
#![feature(custom_test_frameworks)]
#![feature(allocator_api)]
#![test_runner(hootux::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use bootloader_api::entry_point;
use core::alloc::{Allocator, Layout};
use core::mem::{transmute, transmute_copy};
use hootux::exit_qemu;
use hootux::graphics::basic_output::BasicTTY;
use hootux::interrupts::apic::Apic;
use hootux::task::keyboard;
use hootux::task::{executor, Task};
use hootux::time::kernel_init_timer;
use hootux::*;
use log::debug;
use x86_64::VirtAddr;

const BOOT_CONFIG: bootloader_api::BootloaderConfig = {
    use bootloader_api::config::Mapping;
    let mut cfg = bootloader_api::BootloaderConfig::new_default();
    cfg.kernel_stack_size = 0x48000u64;
    cfg.mappings.physical_memory = Some(Mapping::Dynamic);

    cfg
};

entry_point!(kernel_main, config = &BOOT_CONFIG);
#[no_mangle]
fn kernel_main(b: &'static mut bootloader_api::BootInfo) -> ! {
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

        init(); // todo break apart

        mem::set_sys_frame_alloc(&mut b.memory_regions);

        mem::set_sys_mem_tree_no_cr3(mapper);

        allocator::init_comb_heap(0x4444_4000_0000);
        mem::buddy_frame_alloc::drain_map();
    }

    if let bootloader_api::info::Optional::Some(tls) = b.tls_template {
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
    if let Some(buff) =
        core::mem::replace(&mut b.framebuffer, bootloader_api::info::Optional::None).into_option()
    {
        // take framebuffer to prevent aliasing
        graphics::KERNEL_FRAMEBUFFER.init(graphics::FrameBuffer::from(buff));
        graphics::KERNEL_FRAMEBUFFER.get().clear();
        *graphics::basic_output::WRITER.lock() = Some(BasicTTY::new(&graphics::KERNEL_FRAMEBUFFER));
    };

    let acpi_tables = unsafe {
        let t = acpi::AcpiTables::from_rsdp(
            system::acpi::AcpiGrabber,
            *b.rsdp_addr.as_mut().unwrap() as usize,
        )
        .unwrap();
        let fadt = acpi::PlatformInfo::new(&t).unwrap();
        let pmtimer = fadt.pm_timer.expect("No PmTimer found");
        let timer = Box::new(time::acpi_pm_timer::AcpiTimer::locate(pmtimer));
        kernel_init_timer(timer);

        t
    };
    // temporary, until thread local segment is set up
    interrupts::apic::cal_and_run(0x20000, 50);

    init_logger();

    say_hi();

    debug!("Successfully initialized Kernel");

    log::info!("Scanning pcie bus");

    let dev = {
        let pci_cfg = acpi::mcfg::PciConfigRegions::new(&acpi_tables).unwrap();
        system::pci::enumerate_devices(&pci_cfg);
    };

    log::info!("Bus scan complete");

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
    // SAFETY: not safe. no mp yet though.
    unsafe {
        runlevel::set_panic();
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
