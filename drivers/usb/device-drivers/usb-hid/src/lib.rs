#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use core::fmt::Write;
use hootux::fs::file::*;
use hootux::fs::sysfs::SysfsDirectory;
use hootux::fs::*;

const USB_HID_CLASS: u8 = 0x03;

pub mod descriptors;
mod driver_obj;
mod requests;

pub fn init() {
    hootux::task::run_task(Box::pin(init_inner()))
}

async fn init_inner() -> hootux::task::TaskResult {
    // `/sys/bus/event` is guaranteed to be present and NormalFile
    let event_file = cast_file!(NormalFile: SysfsDirectory::get_file(&sysfs::SysFsRoot::new().bus,"event").unwrap() as Box<dyn File>).unwrap();
    loop {
        let event = event_file.read(0, hootux::mem::dma::DmaBuffer::bogus());

        let Ok(usb_bus) = SysfsDirectory::get_file(&sysfs::SysFsRoot::new().bus, "usb") else {
            log::debug!(
                "USB: failed to get /sys/bus/usb\n{:?}",
                SysfsDirectory::file_list(&sysfs::SysFsRoot::new().bus)
            );
            hootux::task::util::sleep(10).await;
            continue;
        };
        let usb_bus = usb_bus.into_sysfs_dir().unwrap();
        'files: for ctl in SysfsDirectory::file_list(&*usb_bus)
            .iter()
            .filter(|n| *n != "." && *n != "..")
        {
            let Ok(controller) = SysfsDirectory::get_file(&*usb_bus, ctl) else {
                // we should log an error on failure, but that may result in shitloads of repeated errors.
                continue 'files;
            };

            let buffer = vec![0u8; 8].into();

            scan_ports(&*controller.into_sysfs_dir().unwrap(), buffer).await;
        }
        let _ = event.await;
    }
}

async fn find_hid_recursive(
    device: Box<dyn SysfsDirectory>,
    mut class_buffer: hootux::mem::dma::DmaBuff,
) -> hootux::mem::dma::DmaBuff {
    let class_f = SysfsDirectory::get_file(&*device, "class").unwrap();
    let class_f = cast_file!(NormalFile: class_f as Box<dyn File>).unwrap();
    let Ok(device): Result<Box<usb::UsbDeviceFile>, _> = device.as_any().downcast() else {
        unreachable!()
    };
    match class_f
        .read(u32::from_le_bytes([3, 0, 0, 0x83]) as u64, class_buffer)
        .await
    {
        Ok((buffer, _)) => {
            class_buffer = buffer;
            let driverstate = Box::new(driver_obj::DriverStateFile::new());

            match device.acquire_ctl(&*driverstate).await {
                Ok(ctl) => driverstate.install_ctl(ctl).await,
                Err(IoError::Busy) => return class_buffer, // already bound
                Err(e) => log::error!("Got {:?} while attempting to bind {}", e, device.device()),
            }

            let rt = driverstate.runtime();
            hootux::task::run_task(Box::pin(rt.run()));
        }
        Err((_, buffer, _)) => {
            // Determine if `device` is a hub
            if let Ok(ports) = SysfsDirectory::get_file(&*device, "ports") {
                class_buffer = scan_ports(&*ports.into_sysfs_dir().unwrap(), buffer).await
            } else {
                class_buffer = buffer;
            }
        }
    }

    class_buffer
}

async fn scan_ports(
    dir: &dyn SysfsDirectory,
    mut class_buffer: hootux::mem::dma::DmaBuff,
) -> hootux::mem::dma::DmaBuff {
    let ports = SysfsDirectory::get_file(dir, "ports")
        .unwrap()
        .into_sysfs_dir()
        .unwrap();
    let mut buffer = String::with_capacity(8);
    for i in 0..127 {
        core::write!(buffer, "{}", i).unwrap();
        let Ok(port) = SysfsDirectory::get_file(&*ports, &buffer) else {
            // File not found
            continue;
        };

        let fut = Box::pin(find_hid_recursive(
            port.into_sysfs_dir().unwrap(),
            class_buffer,
        ));
        class_buffer = fut.await;
    }

    class_buffer
}
