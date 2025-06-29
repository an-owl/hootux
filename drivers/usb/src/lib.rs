#![feature(allocator_api)]
#![no_std]
extern crate alloc;

pub mod ehci {
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use core::ptr::NonNull;
    use ehci::operational_regs::{OperationalRegistersVolatileFieldAccess, UsbStatus};
    use ehci::{
        cap_regs::CapabilityRegisters,
        operational_regs::{OperationalRegisters, PortStatusCtl},
    };
    use volatile::{VolatilePtr, VolatileRef};
    use ehci::frame_lists::{Endpoint, QueueElementTransferDescriptor, QueueHead};
    use hootux::alloc_interface::{DmaAlloc, MmioAlloc};

    pub struct Ehci {
        capability_registers: &'static CapabilityRegisters,
        operational_registers: VolatilePtr<'static, OperationalRegisters>,
        address_bmp: u128,
        ports: Box<[VolatileRef<'static, PortStatusCtl>]>,

    }

    impl Ehci {
        /// This fn constructs a `Controller`
        /// This does not perform any initialisation.
        /// If this fn determines that the entire host controller cannot be accessed then it will return `None`.
        ///
        /// # Safety
        ///
        /// The caller must guarantee that `hci_pointer` outlives the returned `Controller` and that
        /// `hci_pointer` points to a valid Enhanced Host Controller interface.
        ///
        /// The caller should ensure that `hci_pointer` points to the entire region described by BAR0 in
        /// the PCI configuration region.
        pub unsafe fn new(hci_pointer: NonNull<[u8]>) -> Option<Self> {
            let len = hci_pointer.len();
            let op_offset = unsafe { core::ptr::read_volatile(hci_pointer.cast::<u32>().as_ptr()) };
            if op_offset as usize + size_of::<OperationalRegisters>() > len {
                return None;
            }

            // SAFETY: The caller must guarantee that this is safe to deref. These registers are not volatile and can be safely dereferenced.
            let cap_regs = unsafe { hci_pointer.cast::<CapabilityRegisters>().as_ref() };
            // SAFETY: The caller must guarantee that this
            let op_regs = unsafe {
                VolatilePtr::new(NonNull::new(cap_regs.get_operational_registers()).unwrap())
            }; // Pointer was offset from`cap_regs` this cannot be Null (unless it wrapped around I guess)

            let port_count = cap_regs.struct_params.port_count() as usize;
            let last_port = OperationalRegisters::get_port(op_regs.as_raw_ptr(), port_count);

            // SAFETY: The unsafe pointer offsets in this block are safe because the resulting pointers are never dereferenced.
            // The are used to determine that the pointer is valid within `hci_pointer`
            {
                let head = hci_pointer.cast::<u8>().as_ptr();
                let tail = unsafe { head.add(len) };
                let t = unsafe { last_port.add(1).byte_sub(1).cast() };
                if !(head..tail).contains(&t) {
                    return None;
                }
            }

            let mut port_vec = Vec::with_capacity(port_count);
            for portnum in 0..port_count {
                let port_ptr = OperationalRegisters::get_port(op_regs.as_raw_ptr(), portnum);
                // SAFETY: Pointer is valid and exclusive, a major logic error has occured if `port_ptr` is NULL
                port_vec.push(unsafe { VolatileRef::new(NonNull::new_unchecked(port_ptr)) });
            }

            Some(Self {
                capability_registers: cap_regs,
                operational_registers: op_regs,
                address_bmp: 1,
                ports: port_vec.into_boxed_slice(),
            })
        }

        /// Returns an address in the range 1..128
        fn alloc_address(&mut self) -> u8 {
            let bit = self.address_bmp.trailing_ones();
            self.address_bmp.set_bit(bit as usize, true);

            self.address_bmp;
            bit as u8
        }

        /// Frees the given address allocated by [Self::alloc_address]
        ///
        /// # Panics
        ///
        /// This fn will panic if `address == 0` or `address => 128` or if the address is already free
        fn free_address(&mut self, address: u8) {
            assert!(address < 128);
            assert_ne!(address, 0, "Cannot free address 0");
            assert!(
                self.address_bmp.bit(address as usize),
                "Attempted double free"
            );
            self.address_bmp.set_bit(address as usize, false)
        }

        pub fn handle_interrupt(&mut self) {
            use ehci::operational_regs::UsbStatus;
            let sts_reg = self.operational_registers.usb_status();
            let int_status = sts_reg.read();

        for i in int_status.int_only().iter() {
            match i {
                UsbStatus::USB_INT => {
                    sts_reg.write(UsbStatus::USB_INT);
                    todo!()
                }
                UsbStatus::USB_ERROR_INT => {
                    sts_reg.write(UsbStatus::USB_ERROR_INT);
                    todo!()
                }
                UsbStatus::PORT_CHANGE_DETECT => {
                    sts_reg.write(UsbStatus::PORT_CHANGE_DETECT);
                    todo!()
                }
                UsbStatus::FRAME_LIST_ROLLOVER => {
                    sts_reg.write(UsbStatus::FRAME_LIST_ROLLOVER);
                    todo!()
                }
                UsbStatus::HOST_SYSTEM_ERROR => {
                    sts_reg.write(UsbStatus::HOST_SYSTEM_ERROR);
                    todo!()
                }
                UsbStatus::INTERRUPT_ON_ASYNC_ADVANCE => {
                    sts_reg.write(UsbStatus::INTERRUPT_ON_ASYNC_ADVANCE);
                    todo!()
                }
                _ => unreachable!(), // All remaining bits are masked out
            }
        }
    }
}
            for i in int_status.int_only().iter() {
                match i {
                    UsbStatus::USB_INT => {
                        sts_reg.write(UsbStatus::USB_INT);
                        todo!()
                    }
                    UsbStatus::USB_ERROR_INT => {
                        sts_reg.write(UsbStatus::USB_ERROR_INT);
                        todo!()
                    }
                    UsbStatus::PORT_CHANGE_DETECT => {
                        sts_reg.write(UsbStatus::PORT_CHANGE_DETECT);
                        todo!()
                    }
                    UsbStatus::FRAME_LIST_ROLLOVER => {
                        sts_reg.write(UsbStatus::FRAME_LIST_ROLLOVER);
                        todo!()
                    }
                    UsbStatus::HOST_SYSTEM_ERROR => {
                        sts_reg.write(UsbStatus::HOST_SYSTEM_ERROR);
                        todo!()
                    }
                    UsbStatus::INTERRUPT_ON_ASYNC_ADVANCE => {
                        sts_reg.write(UsbStatus::INTERRUPT_ON_ASYNC_ADVANCE);
                        todo!()
                    }
                    _ => unreachable!(), // All remaining bits are masked out
                }
            }
        }
struct Endpoint {
    num: u8
}

impl Endpoint {
    /// Constructs a new Endpoint.
    ///
    /// `num` must be less than 16.
    const fn new(num: u8) -> Option<Self> {
        match num {
            0..16 => Some(Endpoint {num}),
            _ => None,
        }
    }
}

struct Target {
    pub dev: Device,
    pub endpoint: Endpoint,
}



enum PidCode {
    Control,
    In,
    Out,
}