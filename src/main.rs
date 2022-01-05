#![no_std]
#![no_main]
#![feature(const_mut_refs)]

#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

mod vga_text;
mod serial;

#[no_mangle]
pub extern "C" fn _start() -> !{

    println!("hello, World!");
    #[cfg(test)]
    test_main();

    panic!("Almost fell through");
}

pub fn exit_qemu(exit_code: QemuExitCode){
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}





#[cfg(not(test))]
#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> !{

    println!("{}", info);

    loop{}
}

#[cfg(test)]
#[panic_handler]
fn test_panic(info: &core::panic::PanicInfo) -> ! {
    serial_println!("[FAILED]");
    serial_println!("Error: {}", info);
    exit_qemu(QemuExitCode::Failed);
    loop {}
}

#[cfg(test)]
fn test_runner(tests: &[&dyn Testable]){
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run()
    }
    exit_qemu(QemuExitCode::Success);
}






#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode{
    Success = 0x10,
    Failed = 0x11,
}

pub trait Testable{
    fn run (&self);
}

impl<T> Testable for T
    where T: Fn()
{
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[PASSED]");
    }
}
