#![no_std]
#![feature(inline_const_pat)]

extern crate alloc;

use crate::controller::Controller;
use crate::controller::file::PortNum;
use alloc::boxed::Box;

mod controller;

pub extern "C" fn init() {
    log::warn!("[hid-ps2] Initializing 8042 without checking ACPI FADT");
    log::warn!(
        "[hid-ps2] This driver isnt tested on bare metal, command timing may be incorrect, this may lead to incorrect behaviour"
    );

    hootux::task::run_task(Box::pin(i8042_bringup()));
}

async fn i8042_bringup() -> hootux::task::TaskResult {
    use hootux::fs::sysfs;
    let ctl = Controller::new();
    let Ok(ctl) = ctl.init().await else {
        log::warn!("Failed to start i8042"); // not an error, device may not be present.
        return hootux::task::TaskResult::ExitedNormally;
    };

    let major = ctl.major();
    let multi = ctl.is_multi_port();

    let ctl = alloc::sync::Arc::new(async_lock::Mutex::new(ctl));

    let mut l = ctl.lock().await;
    l.cfg_interrupt_port1().await;
    l.init_port1().await;

    if multi {
        l.cfg_interrupt_port2().await;
        ctl.lock().await;
    }

    let sysfs = sysfs::SysFsRoot::new();
    let bus = Clone::clone(&sysfs.bus);

    let port_1 = controller::file::PortFileObject::new(ctl.clone(), PortNum::One, major);
    log::info!("created sys/bus/i8042/port-1");

    bus.insert_device(Box::new(port_1));

    if multi {
        let port_2 = controller::file::PortFileObject::new(ctl.clone(), PortNum::Two, major);
        log::info!("created sys/bus/i8042/port-2");

        bus.insert_device(Box::new(port_2));
    }

    hootux::task::TaskResult::ExitedNormally
}
