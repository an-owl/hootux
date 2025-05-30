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
use core::ptr::NonNull;
use hatcher::boot_info::PixelFormat;
use hootux::exit_qemu;
use hootux::graphics::basic_output::BasicTTY;
use hootux::interrupts::apic::Apic;
use hootux::time::kernel_init_timer;
use hootux::*;
use log::debug;
use x86_64::addr::VirtAddr;

kernel_proc_macro::multiboot2_header! {
    multiboot2_header::HeaderTagISA::I386,
    #[unsafe(link_section = ".multiboot2_header")],
    multiboot2_header::FramebufferHeaderTag::new(multiboot2_header::HeaderTagFlag::Optional,0,0,32)
    Pad::new()
    multiboot2_header::EfiBootServiceHeaderTag::new(multiboot2_header::HeaderTagFlag::Required),
    multiboot2_header::EntryEfi64HeaderTag::new(multiboot2_header::HeaderTagFlag::Required, 0x200000), // address is specified in linker script
    Pad::new(),
}

#[allow(dead_code)]
struct Pad(u32);

impl Pad {
    #[allow(dead_code)] // This absolutely is used
    const fn new() -> Pad {
        Pad(0)
    }
}

#[unsafe(no_mangle)]
fn kernel_main(b: *mut hatcher::boot_info::BootInfo) -> ! {
    serial_println!("Kernel start");
    let mut b = unsafe { b.read() };

    //initialize system
    if let Some(ref mut g) = b.optionals.graphic_info {
        g.framebuffer.fill_with(|| 0xff)
    }

    let mapper;
    unsafe {
        mapper = mem::init(VirtAddr::new(b.physical_address_offset));

        init(); // todo break apart

        mem::set_sys_frame_alloc(b.memory_map.take().unwrap()); // memory map is guaranteed to be present

        mem::set_sys_mem_tree_no_cr3(mapper);

        mem::allocator::init_comb_heap(0x4444_4000_0000);
        mem::buddy_frame_alloc::drain_map();
    }

    if let Some(tls) = b.get_tls_template() {
        // SAFETY: this is safe because the data given is correct
        unsafe { mem::thread_local_storage::init_tls(tls.file.as_ptr(), tls.file.len(), tls.size) }
    }

    mem::init_mm_subsys();

    interrupts::apic::load_apic();
    // SAFETY: prob safe but i dont want to think rn
    unsafe { interrupts::apic::get_apic().set_enable(true) }

    //initialize graphics
    if let Some(buff) = b.optionals.graphic_info.take() {
        let mut fb = NonNull::from(buff.framebuffer);
        mem::write_combining::set_wc_data(&fb).unwrap(); // wont panic

        let pxmode = match buff.pixel_format {
            PixelFormat::Rgb32 => panic!("Big endian is a mental disorder"),
            PixelFormat::Bgr32 => graphics::PixelFormat::Bgr4Byte,
            PixelFormat::ColourMask { .. } => panic!("Custom pixel format specified"),
            _ => panic!("We dont do 24 bit here"),
        };

        // SAFETY: This is safe, we need a NonNull above and are just casting it back.
        graphics::KERNEL_FRAMEBUFFER.init(graphics::FrameBuffer::new(
            buff.width as usize,
            buff.height as usize,
            buff.stride as usize,
            unsafe { fb.as_mut() },
            pxmode,
        ));
        graphics::KERNEL_FRAMEBUFFER.get().clear();
        *graphics::basic_output::WRITER.lock() = Some(BasicTTY::new(&graphics::KERNEL_FRAMEBUFFER));
    };

    let acpi_tables = unsafe {
        let t = if let Some(acpi) = b.rsdp_ptr() {
            acpi::AcpiTables::from_rsdp(system::acpi::AcpiGrabber, acpi.addr()).unwrap()
        } else {
            todo!(); // try alternate ways to locate rsdp
        };
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

    {
        let tmpfs = fs::tmpfs::TmpFsRoot::new();
        fs::init_fs(tmpfs);
        fs::sysfs::init();
        fs::sysfs::SysFsRoot::new().firmware.load_acpi(acpi_tables);
    }

    if let Some(madt) = fs::sysfs::firmware::acpi::get_table::<acpi::madt::Madt>() {
        system::sysfs::get_sysfs().setup_ioapic((&*madt).get());
    } else {
        log::warn!("No MADT was found unable to setup IOAPIC")
    }

    log::info!("Scanning pcie bus");

    // move into task
    if let Some(mcfg) = fs::sysfs::firmware::acpi::get_table::<acpi::mcfg::Mcfg>() {
        system::pci::enumerate_devices(mcfg.entries());
    }
    log::info!("Bus scan complete");

    // SAFETY: MP not initialized, race conditions are impossible.
    #[cfg(test)]
    test_main();

    init_static_drivers();

    #[cfg(feature = "kernel-shell")]
    task::run_task(Box::pin(start_kshell()));

    // Should this be started before or after init_static_drivers()?
    {
        let tls = b.get_tls_template().unwrap();
        unsafe { mp::start_mp(tls.file.as_ptr(), tls.file.len(), tls.size) }
    }

    task::run_exec(); //executor.run();
}

hatcher::kernel_entry!(_hatcher_entry);

#[unsafe(no_mangle)]
pub extern "C" fn _hatcher_entry(bi: *mut hatcher::boot_info::BootInfo) -> ! {
    kernel_main(bi)
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
    hid_ps2::init();
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

#[cfg(feature = "kernel-shell")]
async fn start_kshell() -> task::TaskResult {
    const SHELL_INPUT: &str = "/sys/bus/uart/uart0";
    hootux::task::util::sleep(1000).await;

    let dir = cast_file!(fs::file::Directory: fs::get_vfs().open("/sys/bus/uart").await.unwrap())
        .unwrap();
    for i in dir.file_list().await.unwrap() {
        log::info!("{i}");
    }

    let file = match hootux::fs::get_vfs().open(SHELL_INPUT).await {
        Ok(file) => file,
        Err(e) => {
            log::error!("Failed to open {SHELL_INPUT}: {:?}", e);
            return task::TaskResult::Error;
        }
    };

    let Ok(fifo) = cast_file!(fs::device::Fifo<u8>: file) else {
        log::error!("{SHELL_INPUT} is not a FIFO");
        return task::TaskResult::Error;
    };
    match kshell::KernelShell::new(&*fifo) {
        Ok(kshell) => kshell.run().await,
        Err(e) => {
            log::error!(
                "Failed to spawn kshell, {SHELL_INPUT} returned {:?} when opened",
                e
            );
            task::TaskResult::Error
        }
    }
}
