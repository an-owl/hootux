use crate::system::pci::DeviceControl;

pub fn scan_advanced(mcfg: &[acpi::mcfg::McfgEntry]) {
    for seg in mcfg {
        scan_bus(seg, seg.pci_segment_group, 0);
    }
    //super::PCI_META.write().sort();
}

fn scan_bus(segment_info: &acpi::mcfg::McfgEntry, bus_group: u16, bus: u8) {
    for dev_num in 0..32 {
        let dev_addr = super::DeviceAddress::new(bus_group, bus, dev_num, 0);
        if let Some(phys_addr) = dev_addr.advanced_cfg_addr(segment_info) {
            if let Some(dev) = DeviceControl::new(phys_addr, dev_addr) {
                if dev.is_multi_fn() {
                    check_fns(segment_info, dev.address())
                }

                check_dev(segment_info, dev)
            }
        }
    }
}

fn check_dev(mcfg: &acpi::mcfg::McfgEntry, mut dev: DeviceControl) {
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

    let func_dir = super::file::FuncDir::new(dev);

    hootux::fs::sysfs::SysFsRoot::new()
        .bus
        .insert_device(alloc::boxed::Box::new(func_dir));
}

fn check_fns(mcfg: &acpi::mcfg::McfgEntry, addr: super::DeviceAddress) {
    for i in 1..8 {
        let new_addr = addr.new_function(i);

        if let Some(p_addr) = new_addr.advanced_cfg_addr(mcfg) {
            if let Some(dev) = DeviceControl::new(p_addr, new_addr) {
                check_dev(mcfg, dev)
            }
        }
    }
}
