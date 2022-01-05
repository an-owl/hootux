#![no_std]
#![no_main]
#![feature(const_mut_refs)]

#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

mod vga_text;

#[no_mangle]
pub extern "C" fn _start() -> !{

    println!("hello, World!");
    #[cfg(test)]
    test_main();

    panic!("Almost fell through");
}

#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> !{

    println!("{}", info);

    loop{}
}

#[cfg(test)]
fn test_runner(tests: &[&dyn Fn()]){
    println!("Running {} tests", tests.len());
    for test in tests {
        test()
    }
    exit_qemu(QemuExitCode::Success);
}

#[test_case]
fn test_test(){
    print!("testing test");
    assert_eq!(1,1);
    println!("[ok]");
}


pub fn exit_qemu(exit_code: QemuExitCode){
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode{
    Success = 0x10,
    Failed = 0x11,
}