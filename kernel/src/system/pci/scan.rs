use crate::system::pci::DeviceControl;
use acpi::mcfg::PciConfigRegions;

pub fn scan_advanced(mcfg: &PciConfigRegions<alloc::alloc::Global>) {
    for seg in 0..u16::MAX {
        if let Some(_) = mcfg.physical_address(seg, 0, 0, 0) {
            scan_bus(mcfg, seg, 0);
        } else {
            break;
        }
    }
    //super::PCI_META.write().sort();
}

fn scan_bus(mcfg: &PciConfigRegions<alloc::alloc::Global>, bus_group: u16, bus: u8) {
    for dev_num in 0..32 {
        let dev_addr = super::DeviceAddress::new(bus_group, bus, dev_num, 0);
        if let Some(phys_addr) = dev_addr.advanced_cfg_addr(mcfg) {
            if let Some(dev) = DeviceControl::new(phys_addr, dev_addr) {
                if dev.is_multi_fn() {
                    check_fns(mcfg, dev.address())
                }

                check_dev(mcfg, dev)
            }
        }
    }
}

fn check_dev(mcfg: &PciConfigRegions<alloc::alloc::Global>, mut dev: DeviceControl) {
    log::info!("Discovered PCI Device at: {}", dev.address());
    if dev.dev_type() == super::configuration::register::HeaderType::Bridge {
        let dev_addr = dev.address();
        let h = dev
            .get_config_region()
            .as_any()
            .downcast_ref::<super::configuration::BridgeHeader>()
            .unwrap(); // If this panics the device type has been interpreted incorrectly

        scan_bus(mcfg, dev_addr.as_int().0, h.get_secondary_bus());
    }

    /*
    super::PCI_META
        .write()
        .push(super::meta::MetaInfo::from(&dev));

     */

    let addr = dev.address();
    let dev_ref = alloc::sync::Arc::new(spin::Mutex::new(dev)); // for kernel
    super::PCI_DEVICES.insert(addr, dev_ref.clone());
    crate::system::sysfs::get_sysfs()
        .get_discovery()
        .register_resource(alloc::boxed::Box::new(super::PciResourceContainer::new(
            // for everything else
            addr, dev_ref,
        )));
}

fn check_fns(mcfg: &PciConfigRegions<alloc::alloc::Global>, addr: super::DeviceAddress) {
    for i in 1..8 {
        let new_addr = addr.new_function(i);

        if let Some(p_addr) = new_addr.advanced_cfg_addr(mcfg) {
            if let Some(dev) = DeviceControl::new(p_addr, new_addr) {
                check_dev(mcfg, dev)
            }
        }
    }
}
