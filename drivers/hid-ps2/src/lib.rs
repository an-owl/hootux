#![no_std]
#![feature(inline_const_pat)]

extern crate alloc;

mod controller;

extern "C" fn init() {
    log::warn!("[hid-ps2] Initializing 8042 without checking ACPI FADT");
    log::warn!(
        "[hid-ps2] This driver isnt tested on bare metal, command timing may be incorrect, this may lead to incorrect behaviour"
    )
}
