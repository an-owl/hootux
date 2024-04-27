#![no_std]
#![no_main]
#![feature(const_mut_refs)]
#![feature(custom_test_frameworks)]
#![feature(allocator_api)]
#![test_runner(hootux::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::boxed::Box;
use bootloader_api::entry_point;
use core::ptr::NonNull;
use hootux::exit_qemu;
use hootux::graphics::basic_output::BasicTTY;
use hootux::interrupts::apic::Apic;
use hootux::task::keyboard;
use hootux::time::kernel_init_timer;
use hootux::*;
use log::debug;
use x86_64::VirtAddr;

const BOOT_CONFIG: bootloader_api::BootloaderConfig = {
    use bootloader_api::config::Mapping;
    let mut cfg = bootloader_api::BootloaderConfig::new_default();
    cfg.kernel_stack_size = 0x100000;
    cfg.mappings.physical_memory = Some(Mapping::Dynamic);
    cfg
};

kernel_proc_macro::multiboot2_header! {
    multiboot2_header::HeaderTagISA::I386,
    #[link_section = ".multiboot2_header"],
    multiboot2_header::EfiBootServiceHeaderTag::new(multiboot2_header::HeaderTagFlag::Required),
    multiboot2_header::EntryEfi64HeaderTag::new(multiboot2_header::HeaderTagFlag::Required, 0x200000), // address is specified in linker script
    Pad::new(),
}

#[allow(dead_code)]
struct Pad(u32);

impl Pad {
    const fn new() -> Pad {
        Pad(0)
    }
}

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

        mem::allocator::init_comb_heap(0x4444_4000_0000);
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
        mem::write_combining::set_wc_data(&NonNull::from(buff.buffer())).unwrap(); // wont panic

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
    interrupts::apic::cal_and_run(0x20000);

    init_logger();

    say_hi();

    debug!("Successfully initialized Kernel");

    let madt = acpi_tables.find_table::<acpi::madt::Madt>().unwrap();
    system::sysfs::get_sysfs().setup_ioapic(&madt);

    log::info!("Scanning pcie bus");

    // move into task
    let pci_cfg = acpi::mcfg::PciConfigRegions::new(&acpi_tables).unwrap();
    system::pci::enumerate_devices(&pci_cfg);
    log::info!("Bus scan complete");

    // SAFETY: MP not initialized, race conditions are impossible.gugui
    unsafe { system::sysfs::get_sysfs().firmware().cfg_acpi(acpi_tables) }

    #[cfg(test)]
    test_main();

    init_static_drivers();

    // Should this be started before or after init_static_drivers()?
    {
        let tls = b.tls_template.into_option().unwrap();
        unsafe {
            mp::start_mp(
                tls.start_addr as usize as *const u8,
                tls.file_size as usize,
                tls.mem_size as usize,
            )
        }
    }

    task::run_task(Box::pin(keyboard::print_key()));
    task::run_exec(); //executor.run();
}


libboot::kernel_entry!(_libboot_entry);

#[no_mangle]
pub extern "C" fn _libboot_entry(_bi: libboot::boot_info::BootInfo) -> ! {
    log::info!("libboot worked");
    stop();
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

fn init_static_drivers() {
    serial::init_rt_serial();
    ahci::init();
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