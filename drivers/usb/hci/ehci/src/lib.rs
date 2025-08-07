#![no_std]
#![feature(allocator_api)]

extern crate alloc;

use bitfield::{Bit, BitMut};
use core::mem::offset_of;

pub mod frame_lists;

pub mod cap_regs {
    use bitfield::bitfield;

    #[repr(C)]
    pub struct CapabilityRegisters {
        len: u8,
        _reserved: u8,
        pub struct_params: HcStructParams,
        pub capability_params: HcCapParams,
        pub companion_port_route: CompanionPortRoute,
    }
    impl CapabilityRegisters {
        pub fn get_operational_registers(
            &self,
        ) -> *mut crate::operational_regs::OperationalRegisters {
            // SAFETY: The EHCI spec guarantees that the op-regs will be at cap-regs + len
            unsafe {
                (self as *const Self)
                    .byte_add(self.len as usize)
                    .cast_mut()
                    .cast()
            }
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
    pub enum PortRoutingRules {
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
                1..15 => Self(Some((value - 1) as u8)),
                _ => panic!(
                    "Invalid value for {}: {value}",
                    core::any::type_name::<Self>()
                ),
            }
        }
    }

    bitfield! {
        pub struct HcCapParams(u32);
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
    pub struct CompanionPortRoute(u64);
}

pub mod operational_regs {
    use bitfield::bitfield;
    use core::ptr::NonNull;
    use num_enum::{IntoPrimitive, TryFromPrimitive};
    use volatile::VolatileFieldAccess;
    use volatile::access::ReadWrite;

    #[repr(C)]
    #[derive(VolatileFieldAccess)]
    pub struct OperationalRegisters {
        #[access(ReadWrite)]
        pub usb_command: UsbCommand,
        #[access(ReadWrite)]
        pub usb_status: UsbStatus,
        #[access(ReadWrite)]
        pub int_enable: IntEnable,

        pub frame_index: FrameIndex,

        /// When the controller supports 64bit addressing [crate::cap_regs::HcCapParams::bit_64_addressing]
        /// is set, this sets the high 32bits for the address for all memory structures.
        /// Otherwise this is reserved.
        // so they all need to be within the same 4G region
        // Damn
        #[access(ReadWrite)]
        pub g4_seg_selector: u32,

        /// This register contains the 4KiB aligned physical address of the periodic frame list table.
        #[access(ReadWrite)]
        pub frame_list_addr: u32,

        /// This register contains the lower 32bits of the physical address of the
        #[access(ReadWrite)]
        pub async_list_addr: u32,
        _reserved: [u32; 9],
        #[access(ReadWrite)]
        pub cfg_flags: ConfigureFlag,
    }

    impl OperationalRegisters {
        /// Returns a [PortStatusCtl] for the port `portnum`
        ///
        /// # Safety
        ///
        /// The caller must ensure that the port isn't aliased and that `portnum` [crate::cap_regs::HcStructParams::port_count].
        pub fn get_port(this: NonNull<Self>, portnum: usize) -> *mut PortStatusCtl {
            unsafe { this.offset(1).cast::<PortStatusCtl>().add(portnum).as_ptr() }
        }
    }

    bitfield! {
        #[derive(Copy, Clone)]
        pub struct UsbCommand(u32);
        impl Debug;
        /// When set the controller may execute the schedule.
        /// When this bit is cleared the controller completes the current and any pipelined transactions and then halts.
        /// The controller must halt within 16 micro-frames after this bit is cleared.
        // todo the HC halted bit in the status register indicates that the controller has halted
        pub enable, set_enable: 0;

        /// When a `1` is written the controller immediately resets the internal state to its initial value.
        /// Any USB transactions are immediately terminated.
        ///
        /// The PCI configuration space is not affected by this reset.
        /// The reset process is complete when the value of this register is `0`
        pub get_reset_state, reset: 1;

        /// Indicates the frame list size.
        ///
        /// This value can be modified when [crate::cap_regs::CapabilityRegisters]
        pub from into FrameListSize, frame_list_size, set_frame_list_size: 3,2;

        /// Controls whether the controller skips processing the periodic schedule.
        pub periodic_schedule_enable, set_periodic_schedule_enable: 4;

        /// This controls whether the hos skips processing the asynchronous schedule
        pub async_schedule_enable, set_async_schedule_enable: 5;

        /// When this bit is set the, the next time the async schedule is advanced the controller will raise a
        /// "Interrupt on Async Advance" interrupt. When the interrupt is raised this bit will be cleared.
        ///
        /// # Safety
        ///
        /// This must not be set when [Self::async_schedule_enable] is disabled
        pub get_int_on_async_doorbell, set_int_on_async_doorbell: 6;

        /// (Optional)
        ///
        /// Setting this bit causes the controller to reset without affecting the state of the ports
        /// or relationship to the companion controllers.
        ///
        /// When a read of this bit returns `true` the reset has not completed
        ///
        // Presumably writing 1 to this will start the reset but the spec doesnt say that
        pub light_controller_reset, set_light_controller_reset: 7;

        /// (Optional)
        ///
        /// This indicates the number of asynchronous the controller can execute from a high-speed
        /// queue head before continuing to traverse the asynchronous schedule.
        // fixme What does that even mean?
        pub async_schedule_park_count, set_async_schedule_park_count: 9,8;

        /// (Optional)
        ///
        /// Sets whether [Self::async_schedule_park_count] is enabled.
        pub _,async_schedule_park_enable: 11;


        pub u8, from into InterruptThreshold, get_interrupt_threshold, set_interrupt_threshold: 23,16;
    }

    #[repr(u8)]
    #[derive(Copy, Clone, Debug, PartialEq, Eq, num_enum::TryFromPrimitive)]
    pub enum FrameListSize {
        Elements1024 = 0,
        Elements512 = 1,
        Elements256 = 2,
    }

    impl From<u32> for FrameListSize {
        fn from(value: u32) -> Self {
            (value as u8)
                .try_into()
                .expect("Invalid value for FrameListSize given")
        }
    }

    impl From<FrameListSize> for u32 {
        fn from(value: FrameListSize) -> Self {
            value as u8 as u32
        }
    }

    #[repr(u8)]
    #[derive(Copy, Clone, Debug, Eq, PartialEq, IntoPrimitive)]
    pub enum InterruptThreshold {
        OneFrame = 1,
        TwoFrame = 2,
        FourFrame = 4,
        EightFrame = 8,
        SixteenFrame = 16,
        ThirtyTwoFrame = 32,
        SixtyFourFrame = 64,
    }

    impl From<u8> for InterruptThreshold {
        fn from(value: u8) -> Self {
            match value {
                1 => Self::OneFrame,
                2 => Self::TwoFrame,
                4 => Self::FourFrame,
                8 => Self::EightFrame,
                16 => Self::SixteenFrame,
                32 => Self::ThirtyTwoFrame,
                64 => Self::SixtyFourFrame,
                e => panic!("Invalid value for InterruptThreshold {e}"),
            }
        }
    }

    bitflags::bitflags! {
        // todo rewrite docs
        #[derive(Copy, Clone, Debug, PartialEq)]
        pub struct UsbStatus: u32 {

            /// The Host Controller sets this bit to 1 on the
            /// completion of a USB transaction, which results in the retirement of a Transfer Descriptor
            /// that had its IOC bit set.
            const USB_INT = 1;

            /// The Host Controller sets this bit to 1
            /// when completion of a USB transaction results in an error condition (e.g., error counter
            /// underflow). If the TD on which the error interrupt occurred also had its IOC bit set, both
            /// this bit and USBINT bit are set.
            const USB_ERROR_INT = 1 << 1;

            /// The Host Controller sets this bit to a one when any port
            /// for which the Port Owner bit is set to zero (see Section 2.3.9) has a change bit transition
            /// from a zero to a one or a Force Port Resume bit transition from a zero to a one as a
            /// result of a J-K transition detected on a suspended port. This bit will also be set as a
            /// result of the Connect Status Change being set to a one after system software has
            /// relinquished ownership of a connected port by writing a one to a port's Port Owner bit
            ///
            /// This bit is allowed to be maintained in the Auxiliary power well. Alternatively, it is also
            /// acceptable that on a D3 to D0 transition of the EHCI HC device, this bit is loaded with
            /// the OR of all of the PORTSC change bits (including: Force port resume, over-current
            /// change, enable/disable change and connect status change).
            const PORT_CHANGE_DETECT = 1 << 2;

            /// The Host Controller sets this bit to a one when the
            /// Frame List Index (see Section 2.3.4) rolls over from its maximum value to zero. The
            /// exact value at which the rollover occurs depends on the frame list size. For example, if
            /// the frame list size (as programmed in the Frame List Size field of the USBCMD register)
            /// is 1024, the Frame Index Register rolls over every time FRINDEX[13] toggles. Similarly,
            /// if the size is 512, the Host Controller sets this bit to a one every time FRINDEX[12]
            /// toggles.
            const FRAME_LIST_ROLLOVER = 1 << 3;

            /// The Host Controller sets this bit to 1 when a serious error
            /// occurs during a host system access involving the Host Controller module. In a PCI
            /// system, conditions that set this bit to 1 include PCI Parity error, PCI Master Abort, and
            /// PCI Target Abort. When this error occurs, the Host Controller clears the Run/Stop bit in
            /// the Command register to prevent further execution of the scheduled TDs.
            const HOST_SYSTEM_ERROR = 1 << 4;

            /// System software can force the host
            /// controller to issue an interrupt the next time the host controller advances the
            /// asynchronous schedule by writing a one to the Interrupt on Async Advance Doorbell bit
            /// in the USBCMD register. This status bit indicates the assertion of that interrupt source.
            const INTERRUPT_ON_ASYNC_ADVANCE = 1 << 5;

            /// This bit is a zero whenever the Run/Stop bit is a one. The
            /// Host Controller sets this bit to one after it has stopped executing as a result of the
            /// Run/Stop bit being set to 0, either by software or by the Host Controller hardware (e.g.
            /// internal error).
            const CONTROLLER_HALTED = 1 << 12;

            /// This is a read-only status bit, which is used to detect an empty asynchronous schedule
            const RECLIMATION = 1 << 13;

            ///  The bit reports the current real status of
            /// the Periodic Schedule. If this bit is a zero then the status of the Periodic Schedule is
            /// disabled. If this bit is a one then the status of the Periodic Schedule is enabled. The
            /// Host Controller is not required to immediately disable or enable the Periodic Schedule
            /// when software transitions the Periodic Schedule Enable bit in the USBCMD register.
            /// When this bit and the Periodic Schedule Enable bit are the same value, the Periodic
            /// Schedule is either enabled (1) or disabled (0).
            const PERIODIC_SCHEDULE_STATUS = 1 << 14;

            /// The bit reports the current real
            /// status of the Asynchronous Schedule. If this bit is a zero then the status of the
            /// Asynchronous Schedule is disabled. If this bit is a one then the status of the
            /// Asynchronous Schedule is enabled. The Host Controller is not required to immediately
            /// disable or enable the Asynchronous Schedule when software transitions the
            /// Asynchronous Schedule Enable bit in the USBCMD register. When this bit and the
            /// Asynchronous Schedule Enable bit are the same value, the Asynchronous Schedule is
            /// either enabled (1) or disabled (0).
            const ASYNC_SCHEDULE_STATUS = 1 << 15;
        }
    }

    impl UsbStatus {
        /// Masks out bits that do not reflect an interrupt status.
        pub fn int_only(self) -> Self {
            self & (Self::USB_INT
                | Self::USB_ERROR_INT
                | Self::PORT_CHANGE_DETECT
                | Self::FRAME_LIST_ROLLOVER
                | Self::HOST_SYSTEM_ERROR
                | Self::INTERRUPT_ON_ASYNC_ADVANCE)
        }
    }

    bitflags::bitflags! {
        /// See [UsbStatus] for corresponding interrupt meanings.
        ///
        /// Bits in this struct allow the controller to raise the corresponding interrupt.
        #[derive(Copy, Clone, Debug)]
        pub struct IntEnable: u32 {
            const USB_INT = 1;
            const USB_ERROR_INT = 1 << 1;
            const PORT_CHANGE_DETECT = 1 << 2;
            const FRAME_LIST_ROLLOVER = 1 << 3;
            const HOST_SYSTEM_ERROR = 1 << 4;
            const INTERRUPT_ON_ASYNC_ADVANCE = 1 << 5;
        }
    }
    // fixme I didnt really understand the description of this register
    // Only bits 13:0 are readable.
    // This is only writable when the controller is halted and the least 3 significant bits must never be set to `111` or `000`
    pub struct FrameIndex(u32);

    #[repr(u32)]
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    pub enum ConfigureFlag {
        RoutePortsToCompanions = 0,
        RoutePortsToSelf = 1,
    }

    bitfield! {
        #[derive(Copy, Clone, PartialEq)]
        pub struct PortStatusCtl(u32);
        impl BitAnd;
        impl BitOr;
        impl Debug;

        /// Indicates whether the port is currently connected to a device.
        ///
        /// This will remain cleared while [port_power] is also cleared.
        pub connected,_: 0;

        /// Indicates that the port status has changed.
        /// The controller sets this bit for all changes to the ports current connection status.
        ///
        /// This should be acknowledged immediately by writing back [Self::Ack_Status_change].
        pub mask ACK_STATUS_CHANGE(u32), connect_status_change,_: 1;

        /// Ports can only be enabled as a part of the reset-and-enable.
        /// Software cannot set this bit.
        /// This will be set when the controller determines there is a device attached to this port.
        ///
        /// Ports can be disabled by a a fault condition or by software. The value of this bit does
        /// not change until the port state changes.
        pub enabled,set_enabeld: 2;

        /// Reflects whether [Self::enabled] has changed state.
        ///
        /// This is cleared by writing `1` to it.
        pub mask ACK_PORT_ENABLE(u32), port_enable_change, clear_port_enable_change: 3;

        /// This port currently has an over-current condition.
        pub over_current,_: 4;

        /// Reflects whether [Self::over_current] has changed state
        ///
        /// This is cleared by writing `1` to it
        pub mask ACK_OVER_CURRENT(u32), over_current_change, clear_over_current_change: 5;

        /// When [Self::is_suspended] is `true` this will signal a resume to the port.
        ///
        /// Hardware may set this to `true` when a "J-to-K transition" is detected while the port
        /// is in the suspended state. When this occurs [UsbStatus::PORT_CHANGE_DETECT] will be set.
        /// This will not occur when this is triggered by software.
        ///
        /// # Safety
        ///
        /// Setting this to `true` when [Self::is_suspended] is `false` will cause UB.
        pub force_resume,set_force_resume: 6;

        /// When this is set propagation of data down the port is blocked except for port reset.
        /// The suspend occurs at the end of the current transaction.
        /// The bit state does not change until after the port has entered the suspended state,
        /// which may be after the current transaction.
        ///
        /// This bit will be cleared when `self.set_force_resume(true)` is called or this port is reset.
        ///
        /// Note that `set_suspend(false)` has no effect
        pub is_suspended,suspend: 7;

        /// When this is set the bus reset sequence is run, this can be be terminated by writing
        /// clearing this bit. Software must keep this bit set long enough to ensure the reset sequence.
        /// When this is set, [Self::set_enabled] must be cleared.
        ///
        /// When this bit is cleared there may be a delay before it reads `0`.
        /// When the reset process is complete if this port is in high-speed mode the the controller
        /// will enable this port. When this bit is cleared the controller must complete the reset
        /// process within 2milliseconds.
        pub mask RESET(u32), reset_in_progress, reset: 8;

        /// Represents the D+ (high bit) and D- (low bit) lines of the port.
        /// These are used for the detection of a low speed device prior to reset and enable.
        /// This field is only valid when [Self::enable] is `0` and [Self::connect] is `1`.
        pub into LineStatus, line_state,_: 11,10;

        /// When [crate::cap_regs::CapabilityRegisters::port_power_control_enabled] is set this bit
        /// controls the internal power switch for the port. When `port_power_control_enabled` is
        /// clear this bit is always `1` (enabled).
        ///
        /// When an over-current state is detected this bit my be cleared, disabling power to the device.
        // pp lol
        pub port_power, set_port_power: 12;

        /// When this is bit is cleared *this* controller has ownership of this port.
        /// When this bit is cleared the controller hands off the port to a companion controller.
        ///
        /// This bit is set to `1` when [ConfigureFlag::RoutePortsToCompanions] is set.
        pub port_owner, set_port_owner: 13;

        /// When [crate::cap_regs::HcStructParams::port_indicator] is set this sets the port indictor state.
        pub from into PortLed, port_led_state, set_port_led: 15,14;

        /// This sets a test mode for the port.
        pub from into TestCtl, get_port_test_ctl, set_port_test: 19,16;

        /// This bit determines whether this port is sensitive to wakeup events.
        pub get_wake_on_connect, wake_on_connect: 20;

        /// This bit indicates whether this port is sensitive to disconnects as wakeup events.
        pub get_wake_on_disconnect, wake_on_disconnect: 21;

        /// This bit indicates whether this port is sensitive to overcurrents as wakeup events.
        pub get_wake_on_overcurrent, wake_on_overcurrent: 22;
    }

    #[repr(u8)]
    #[derive(Debug, Copy, Clone, PartialEq, Eq, TryFromPrimitive)]
    pub enum LineStatus {
        SE0 = 0b00,
        KState = 0b01,
        JState = 0b10,
        Undefined = 0b11,
    }

    impl From<u32> for LineStatus {
        fn from(value: u32) -> Self {
            (value as u8).try_into().unwrap()
        }
    }

    #[repr(u8)]
    #[derive(Debug, Copy, Clone, PartialEq, Eq, TryFromPrimitive)]
    pub enum PortLed {
        Disabled,
        Amber,
        Green,
    }
    impl From<u32> for PortLed {
        fn from(value: u32) -> Self {
            (value as u8).try_into().unwrap()
        }
    }

    impl Into<u32> for PortLed {
        fn into(self) -> u32 {
            self as u8 as u32
        }
    }

    #[repr(u8)]
    #[derive(Debug, Copy, Clone, PartialEq, Eq, TryFromPrimitive)]
    pub enum TestCtl {
        Disabled = 0,
        TestJState,
        TestKState,
        TestSe0Nak,
        TestPacket,
        TestForceEnable,
    }

    impl From<u32> for TestCtl {
        fn from(value: u32) -> Self {
            (value as u8).try_into().unwrap()
        }
    }

    impl Into<u32> for TestCtl {
        fn into(self) -> u32 {
            self as u8 as u32
        }
    }
}

/// This register is located in the PCI configuration region of the controller,
/// it can be located at the offset [cap_regs::HcCapParams::extended_capabilities].
///
/// This register contains 2 semaphore bits indicating ownership of the controller.
/// When [Self::bios_semaphore] is set the controller is owned by the device firmware and may
/// not be operated by the OS.
/// When [Self::os_semaphore] is set the firmware must release control of the controller,
/// when this precess is completed `Self::bios_semaphore` will be cleared.
///
// The specification indicates this is a part of a list, but idk which one.
// `id` should be `1` but that isn't defined in the PCI spec.

#[repr(C)]
pub struct LegacySupportRegister {
    id: u8,
    next_capability: u8,
    bios_semaphore: bool,
    os_semaphore: bool,
}

impl LegacySupportRegister {
    /// ## Safety
    ///
    /// See [core::ptr::write]
    pub unsafe fn set_os_semaphore(this: *mut Self) {
        // SAFETY: This is a field access into `this`.
        // The caller ensures the pointer points to Self
        unsafe {
            let os_sem = this.byte_add(offset_of!(Self, os_semaphore)).cast::<bool>();
            os_sem.write_volatile(true);
        }
    }

    /// ## Safety
    ///
    /// See [core::ptr::read]
    pub unsafe fn wait_for_release(this: *mut Self) {
        // SAFETY: This is a field access into `this`.
        // The caller ensures the pointer points to Self
        unsafe {
            let os_sem = this
                .byte_add(offset_of!(Self, bios_semaphore))
                .cast::<bool>();
            while !Self::is_os_owned(this) {
                core::hint::spin_loop();
            }
        }
    }

    /// ## Safety
    ///
    /// See [core::ptr::read]
    pub unsafe fn is_os_owned(this: *mut Self) -> bool {
        // SAFETY: This is a field access into `this`.
        // The caller ensures the pointer points to Self
        unsafe {
            let os_sem = this.byte_add(offset_of!(Self, os_semaphore)).cast::<bool>();
            let fw_sem = this
                .byte_add(offset_of!(Self, bios_semaphore))
                .cast::<bool>();
            os_sem.read_volatile() && !fw_sem.read_volatile()
        }
    }
}
