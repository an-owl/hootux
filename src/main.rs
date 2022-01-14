#![no_std]
#![no_main]
#![feature(const_mut_refs)]
#![feature(custom_test_frameworks)]
#![test_runner(owl_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use owl_os::*;
use bootloader::entry_point;
use x86_64::structures::paging::PageTable;
use x86_64::VirtAddr;
use owl_os::mem::active_l4_table;


entry_point!(kernel_main);
#[no_mangle]
fn kernel_main(b: &'static bootloader::BootInfo) -> ! {
    //initialize system
    init();

    println!("hello, World!");

    let phy_mem_offset = VirtAddr::new(b.physical_memory_offset);
    let l4_table = unsafe {active_l4_table(phy_mem_offset)};

    for (i, entry) in l4_table.iter().enumerate(){
        if !entry.is_unused(){
            println!("Entry {}: {:?}", i, entry);

            let phys = entry.frame().unwrap().start_address();
            let virt = phys.as_u64() + b.physical_memory_offset;
            let ptr = VirtAddr::new(virt).as_mut_ptr();
            let l3_table: &PageTable = unsafe {&*ptr};

            for (i,entry) in l3_table.iter().enumerate(){
                if !entry.is_unused(){
                    println!("\t L3 Entry {}: {:?}",i,entry);
                }
            }

        }
    }

    #[cfg(test)]
    test_main();

    stop()
}

#[cfg(not(test))]
#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    println!("KERNEL PANIC\nInfo: {}", info);

    loop {}
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
