#![no_std]


pub struct Echi {
    cap: &'static cap_regs::CapabilityRegisters
}

mod cap_regs {
    use bitfield::bitfield;

    #[repr(C)]
    pub(crate) struct CapabilityRegisters {
        len: u8,
        _reserved: u8,
        struct_params: HcStructParams,
        capability_params: HcCapParams,
        companion_port_route: CompanionPortRoute,
    }
    impl CapabilityRegisters {
        fn get_operational_registers(&self) -> *mut _ {
            unsafe { (self as *const Self).byte_add(self.len as usize).cast_mut() }
        }
    }

    bitfield! {
        pub struct HcStructParams(u32);
        impl Debug;
        /// Indicates the number of ports present on the controller.
        ///
        /// This field is never `0`
        pub port_count,_: 3, 0;
        /// When this field is true this indicates that ports have power switches.
        // Like, physical ones?
        pub port_power_control_enabled, _: 4;
        /// See [PortRoutingRules]
        pub into PortRoutingRules, port_routing_rules, _: 7;
        /// Indicates the number of ports each companion controller supports.
        pub ports_per_companion,_: 11,8;
        /// Indicates the number of companion controllers present for this controller group.
        pub number_of_companions,_: 15,12;
        // TODO link the register fields that this indicates are present
        pub port_indicator,_: 16;
        /// See [DebugPort]
        pub into DebugPort, debug_port_number,_: 23,20;
    }

    // TODO: What do these mean?
    #[repr(u8)]
    #[derive(Copy, Clone, Debug, PartialEq, Eq, num_enum::TryFromPrimitive)]
    enum PortRoutingRules {
        /// The first NPcc ports are routed to the lowest numbered function
        /// companion host controller, the next NPcc port are routed to the next
        /// lowest function companion controller, and so on.
        NPcc = 0,

        /// The port routing is explicitly enumerated by the first N_PORTS elements
        /// of the HCSP-PORTROUTE array.
        NPorts,
    }

    impl From<bool> for PortRoutingRules {
        fn from(value: bool) -> Self {
            (value as u8).try_into().unwrap() // bool is guaranteed to be either 0 | 1, thus this will always succeed
        }
    }


    /// Indicates the port used for debug port and whether it is present.
    #[derive(Debug)]
    pub struct DebugPort(pub Option<u8>);

    impl From<u32> for DebugPort {
        fn from(value: u32) -> Self {
            match value {
                0 => Self(None),
                1..15 => Self(Some((value-1) as u8)),
                _ => panic!("Invalid value for {}: {value}", core::any::type_name::<Self>())
            }
        }
    }


    bitfield! {
        struct HcCapParams(u32);
        impl Debug;

        /// Indicates whether the controller supports 64it addressing.
        pub bit_64_addressing,_: 0;
        /// When set this indicates that the frame list size can be programmed.
        ///
        /// When this is clear the frame list size is always 1024 elements.
        // todo docs indicate what register controls this it should be linked here
        pub programmible_frame_list,_: 1;

        /// Indicates whether the asynchronous park feature is present on this device for high-speed
        /// queue heads in the asynchronous schedule.
        // fixme wtf did i just write?
        pub asynchronous_park, _: 2;

        /// This field indicates, relative to the current position of the executing host controller, where software
        /// can reliably update the isochronous schedule. When bit [7] is zero, the value of the least
        /// significant 3 bits indicates the number of micro-frames a host controller can hold a set of
        /// isochronous data structures (one or more) before flushing the state. When bit [7] is a
        /// one, then host software assumes the host controller may cache an isochronous data
        /// structure for an entire frame.
        // fixme this is just copied from the spec, I dont actually know what it means
        pub isochonous_scheule_threshold,_: 7,4;

        /// Indicates the offset of the extended capabilities list within the PCI configuration region.
        ///
        /// A value of 0 indicates the capability is not present. When the value is not 0 it must be >40.
        // todo change to Option<non-primitive>
        pub extended_capabilities,_: 15,8;
    }

    // properly define this
    struct CompanionPortRoute(u64);
}