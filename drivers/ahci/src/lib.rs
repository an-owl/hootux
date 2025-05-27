#![feature(allocator_api)]
#![feature(int_roundings)]
#![no_std]
extern crate alloc;

use alloc::boxed::Box;

pub mod driver;
pub(crate) mod hba;
pub(crate) mod register;

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    hootux::task::run_task(Box::pin(driver::init_async()));
}

/// This enum is to represent the last known device state.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Copy, Clone)]
enum PortState {
    /// The port is not implemented by the hba
    NotImplemented,
    /// No device has been detected connected to this port
    None,
    /// A device has been detected by cold presence detection.
    ///
    /// This state is only valid when the port supports cold presence detection. Otherwise the state will be [Self::None]
    Cold,
    /// A device is connected and is in a low power state which cannot respond to all commands.
    Warm,
    /// A device is connected and ready to receive commands.
    Hot,
}
