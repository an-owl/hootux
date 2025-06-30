#![no_std]
#![feature(allocator_api)]
extern crate alloc;

pub mod ehci {
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use bitfield::{Bit, BitMut};
    use core::ptr::NonNull;
    use derivative::Derivative;
    use ehci::frame_lists::{QueueElementTransferDescriptor, QueueHead};
    use ehci::operational_regs::OperationalRegistersVolatileFieldAccess;
    use ehci::{
        cap_regs::CapabilityRegisters,
        operational_regs::{OperationalRegisters, PortStatusCtl},
    };
    use hootux::alloc_interface::DmaAlloc;
    use volatile::{VolatilePtr, VolatileRef};

    pub struct Ehci {
        capability_registers: &'static CapabilityRegisters,
        operational_registers: VolatilePtr<'static, OperationalRegisters>,
        address_bmp: u128,
        ports: Box<[VolatileRef<'static, PortStatusCtl>]>,
        async_list: Vec<alloc::sync::Arc<spin::Mutex<EndpointQueue>>>,
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
                async_list: alloc::collections::BTreeSet::new(),
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

        async fn init_port(&mut self, port: usize) -> Option<crate::Device> {
            let port = &mut self.ports[port];
            let mut current = port.as_mut_ptr().read();

            // Assert reset for 50ms
            current.reset(true);
            port.as_mut_ptr().write(current);
            hootux::task::util::sleep(50).await;
            current.reset(false);
            port.as_mut_ptr().write(current);

            todo!()
        }

        fn insert_into_async(&mut self, queue: alloc::sync::Arc<spin::Mutex<EndpointQueue>>) {
            let l = queue.lock();
            let addr: u32 = hootux::mem::mem_map::translate_ptr(self.async_list.first().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            l.head.set_next_queue_head(addr);

            let addr: u32 = hootux::mem::mem_map::translate_ptr(&*l.head)
                .unwrap()
                .try_into()
                .unwrap();

            self.async_list
                .last()
                .unwrap()
                .lock()
                .head
                .set_next_queue_head(addr);

            drop(l);
            self.async_list.push(queue)
        }

        fn init_ports(&mut self) {}
    }

    /// The EndpointQueue maintains the state of queued operations for asynchronous jobs.
    ///
    /// The EndpointQueue operates using [TransactionString]'s, which each describes a queued operation.
    #[derive(Derivative)]
    #[derivative(Ord, PartialEq, PartialOrd, Eq)]
    struct EndpointQueue {
        // For some reason the PID is in the QTD, not the QueueHead so we need to keep the PID
        // I'm sure I'll figure out why soon enough
        #[derivative(PartialEq = "ignore")]
        #[derivative(Ord = "ignore")]
        #[derivative(PartialOrd = "ignore")]
        pid: crate::PidCode,
        target: crate::Target,
        #[derivative(PartialEq = "ignore")]
        #[derivative(Ord = "ignore")]
        #[derivative(PartialOrd = "ignore")]
        head: Box<QueueHead, DmaAlloc>,
        #[derivative(PartialEq = "ignore")]
        #[derivative(Ord = "ignore")]
        #[derivative(PartialOrd = "ignore")]
        work: Vec<TransactionString>,
        // Option because this isn't required when transactions are running.
        // This can be used as a cached descriptor
        #[derivative(PartialEq = "ignore")]
        #[derivative(Ord = "ignore")]
        #[derivative(PartialOrd = "ignore")]
        terminator: Option<Box<QueueElementTransferDescriptor, DmaAlloc>>,
    }

    impl EndpointQueue {
        fn new(target: super::Target, pid: super::PidCode) -> Self {
            let mut this = Self {
                pid,
                target,
                head: Box::new_in(
                    QueueHead::new(),
                    DmaAlloc::new(hootux::mem::MemRegion::Mem32, 32),
                ),
                work: Vec::new(),
                terminator: Some(Box::new_in(
                    QueueElementTransferDescriptor::new(),
                    DmaAlloc::new(hootux::mem::MemRegion::Mem32, 32),
                )),
            };

            // SAFETY: The address is a valid terminated table.
            unsafe {
                this.head.set_current_transaction(
                    hootux::mem::mem_map::translate(
                        this.terminator.as_ref().unwrap() as *const _ as usize
                    )
                    .expect("What? Static isn't mapped?") as u32,
                )
            };

            this.head
                .set_target(target.try_into().expect("Invalid target"));
            this
        }

        fn head_of_list() -> Self {
            let mut this = Self::new(
                crate::Target {
                    dev: crate::Device::Broadcast,
                    endpoint: crate::Endpoint::new(0).unwrap(),
                },
                crate::PidCode::Control,
            );
            this.head.set_head_of_list();
            this
        }
    }

    /// A `TransactionString` describes a series of expected transactions.
    /// Due to the limited buffer size of a single [QueueElementTransferDescriptor] a large number
    /// of them may be required for a single expected operation.
    struct TransactionString {
        // This can be changed into `Box<[QTD],DmaAlloc>` which may be more optimal
        // current form is more flexible
        // May never have 0 elements
        str: Box<[Box<QueueElementTransferDescriptor, DmaAlloc>]>,
    }

    impl TransactionString {
        fn new() -> Self {
            todo!()
        }

        /// When this string has completed `other` will be run.
        ///
        /// When this is called `other` will be loaded into the final `QTD.next` field and all
        /// `QTD.alternate` fields
        ///
        /// # Safety
        ///
        /// The caller must guarantee that when `self` may be executed that `other` will not be
        /// dropped until it's completed execution or the controller is stopped.
        unsafe fn append_string(&mut self, other: &Self) {
            // unwrap: other.str[0] was not mapped into memory?
            let next_addr: u32 = hootux::mem::mem_map::translate_ptr(&*other.str[0])
                .unwrap()
                .try_into()
                .expect("QueueTransportDescriptor mapped into Mem64 memory");
            let tail = self.str.last_mut().unwrap();
            // SAFETY: Next addr is guaranteed to point to point to a valid QTD
            // The caller must guarantee that `other` remains valid
            unsafe {
                tail.set_next(Some(next_addr));
                tail.set_alternate(Some(next_addr));
            };

            for i in &mut self.str {
                // SAFETY: Next addr is guaranteed to point to point to a valid QTD
                // The caller must guarantee that `other` remains valid
                unsafe { i.set_alternate(Some(next_addr)) };
            }
        }
    }
}

/// Represents a target device address. USB address are `0..128`
///
/// The address `0` is the default address which must be responded to.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
enum Device {
    Broadcast,
    Address(core::num::NonZeroU8),
}

impl Device {
    /// Attempts to construct a new device from a `u8`.
    ///
    /// * Addresses bust be `<128`
    /// * `0` is the broadcast address
    const fn new(address: u8) -> Option<Self> {
        match address {
            0 => Some(Device::Broadcast),
            n @ 1..64 => Some(Self::Address(core::num::NonZeroU8::new(n).unwrap())), // `n` is not one
            _ => None,
        }
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
struct Endpoint {
    num: u8,
}

impl Endpoint {
    /// Constructs a new Endpoint.
    ///
    /// `num` must be less than 16.
    const fn new(num: u8) -> Option<Self> {
        match num {
            0..16 => Some(Endpoint { num }),
            _ => None,
        }
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
struct Target {
    pub dev: Device,
    pub endpoint: Endpoint,
}

impl TryFrom<Target> for ::ehci::frame_lists::Target {
    type Error = ();
    fn try_from(value: Target) -> Result<Self, Self::Error> {
        // UEB 2.0 only allows addresses < 64
        // IDK about other revisions, they may allow more
        let addr_raw = match value.dev {
            Device::Address(addr) if addr.get() >= 64 => return Err(()),
            Device::Address(addr) => addr.get(),
            Device::Broadcast => 0,
        };

        Ok(Self {
            address: ::ehci::frame_lists::Address::new(addr_raw),
            endpoint: ::ehci::frame_lists::Endpoint::new(value.endpoint.num),
        })
    }
}

#[derive(Debug, Copy, Clone)]
enum PidCode {
    Control,
    In,
    Out,
}
