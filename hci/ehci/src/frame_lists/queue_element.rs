use bitfield::bitfield;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use crate::frame_lists::TransactionDirection;

#[repr(C)]
struct QueueHead {
    next_link_ptr: super::FrameListLinkPointer,
    ctl0: Ctl0,
    ctl1: Ctl1,
    /// The lower 32bits of the address of the transaction descriptor.
    /// This must be aligned to 32.
    current_transaction: super::FrameListLinkPointer,
    next_pointer: super::FrameListLinkPointer,
    alternate_pointer: FrameListLinkPointerWithNakCount,
}

bitfield! {
    struct Ctl0(u32);
    impl Debug;
    
    /// See [super::Address]
    get_addr,set_addr: 6,0;
    
    /// When this queue head is in the periodic schedule list and the [Self::endpoint_speed] field indicates a full 
    /// or low speed device this bit will clear the active bit after the next transaction.
    ///
    /// # Safety
    /// 
    /// It is UB to set this bit when this queue head is used in the asynchronous queue or when 
    /// the [Self::endpoint_speed] indicates this is a high speed device.
    // todo what the hell is EPS? + Which interrupt + 
    _,set_inactive_on_next_transaction: 7;
    
    /// Indicates the endpoint speed
    from into EndpointSpeed, endpoint_speed,set_endpoint_speed: 13,12;
    
    /// This bit specifies where the host controller should get the
    /// initial data toggle on an overlay transition.
    /// 
    /// When set initial data toggle comes from the incoming qTD DT bit.
    /// The controller replaces DT bit in the queue head from the DT bit in the qTD.
    /// 
    /// When clear ignore the DT bit from th incoming qTD DT bit in the queue head.
    // todo what is a qTD + link it
    get_data_toggle_ctl, use_dt_from_qtd: 14;
    
    /// This is set to mark a queue head as the head of the reclamation list.
    // todo what does that mean?
    is_head_reclimation_list,set_head_reclimation_list: 15;
    
    /// This field sets the maximum packet length.
    max_packet_len,  set_max_packet_len: 16,26;
    
    /// If [Self::endpoint_speed] is not [EndpointSpeed::FullSpeed] and [Self::get_addr] is `0` then this must be set.
    /// Otherwise it must be cleared
    _, set_control_endpoint_flags: 27;
    
    /// The value in this field is used by the controller to reload the Nak counter field.
    // todo when is it reloaded, what is it for and why is this important.
    nak_count_reload, set_nak_count_reload: 28,31;
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, TryFromPrimitive, IntoPrimitive)]
enum EndpointSpeed {
    FullSpeed = 0,
    LowSpeed = 1,
    HighSpeed = 2,
}

impl From<u32> for EndpointSpeed {
    fn from(value: u32) -> Self {
        (value as u8).try_into().expect("invalid value or endpoint speed")
    }
}

impl From<EndpointSpeed> for u32 {
    fn from(value: EndpointSpeed) -> Self {
        value as u8 as u32
    }
}

bitfield! {
    struct Ctl1(u32);
    impl Debug;
    
    /// This field must be set to `0` when this queue head is on the asynchronous schedule.
    /// Setting this field to a non-zero value indicates this is an interrupt queue head.
    /// 
    /// The lower 3 bits of [super::super::operational_regs::FrameIndex] are a index into this field.
    /// When the index points to a `1` bit then this queue head is a candidate for execution.
    /// 
    /// If the [Ctl0::endpoint_speed] field indicates this is [EndpointSpeed::HighSpeed] then the 
    /// transaction executed is determined by the PID_code field contained in the execution area.
    /// 
    // todo link PID_code field
    /// # Safety
    /// 
    /// This field must be `0` when this [QueueHead] is in the asynchronous list.
    // fixme something about split transactions.
    u8, inerrupt_schedule_mask, set_inerupt_schedule_mask: 7,0;
    
    /// This field is ignored unless this this is a split transaction and this queue head is on the periodic list.
    /// This field is used by the controller with the Active and SplitX-state fields to determine 
    /// which microframes the host controller should a execute split-transaction. When this field 
    /// may be used this field must not be zero.
    /// 
    /// This field is used the same as [Self::interrupt_schedule_mask]. 
    // todo link active and splitx-state
    u8, split_completion_mask, set_split_completion_mask: 8,15;
    
    /// This field is used for split transactions. This field indicates the USB hub that bridges 
    /// into the full/low speed bus.
    u8, into super::Address, _ ,set_bridge_hub: 22,16;
    
    /// When this queue head indicates a split transaction this field indicates the port number of 
    /// the bridge-hub that the target is attached to. 
    u8, _, set_port_number: 19,23;
    
    /// Indicates the number of successive transactions the controller may send to this endpoint per
    /// microframe.
    // fixme: I think. Double check this.
    from into super::TransactionCount, get_endpoint_transaction_count, set_endpoint_transaction_count: 31,30;
}

bitfield::bitfield! {
    struct FrameListLinkPointerWithNakCount(u32);
    impl Debug;

    /// When set this bit indicates that indicates that [Self::ptr] is invalid
    end,set_end: 0;
    /// This is decremented when the transaction returns NAK or NYET. This is reloaded from 
    /// [Ctl0::nak_count_reload] before a transaction is executed during the first pass of the 
    /// reclamation list. It is also reloaded during an overlay.
    // fixme: I have no idea when this is reloaded
    nak_count, set_nak_count: 4,1;
    ptr,set_ptr: 31,5;
}


#[repr(C,align(32))]
struct QueueElementTransferDescriptor {
    next_qtd: super::FrameListLinkPointer,
    /// This field points to the qTD that is used when in the event that the current qTD execution 
    /// encounters a short packet for an [super::TransactionDirection::In] transaction.
    alt_qtd: super::FrameListLinkPointer,
    config: QtdConfig,
    buffer_offset: BufferOffset,
    buffers: [BufferField;4]
}


bitfield! {
    struct QtdConfig(u32);
    impl Debug;

    /// When [Ctl0::endpoint_speed] of the [QueueHead] is [EndpointSpeed::HighSpeed] and 
    /// [Self::get_pid_code] is [PidCode::Out] then this is the state for the ping protocol.
    ///
    /// When this is set the controller will precede an OUT transaction with a PING.
    /// When this bit is clear the controller will not precede a OUT transactions with PING
    /// 
    /// A PING transaction will determine return NAK if the next OUT transaction cannot currently
    /// be received by the endpoint.
    get_ping_state,ping: 0;
    
    /// This bit is not used unless the [QueueHead]'s [Ctl0::endpoint_speed] is not [EndpointSpeed::FullSpeed]
    ///
    /// The controller uses this bit to track the state of the the split transaction. The 
    /// requirements for the state of this bit are determined by whether this transaction is on the 
    /// periodic or asynchronous schedule.
    ///
    /// When this bit is set the controller issues a complete split transaction to the the endpoint.
    /// Wen this bit is clear the controller will issue a start split transaction to the endpoint.
    // I dont think this actually matters.   
    split_transaction_state,_: 1;
    
    /// This bit is ignored if this transaction is [EndpointSpeed::HighSpeed]. 
    /// 
    /// This bit indicates that the controller detected a host-induced hold-off caused the host 
    /// controller to mis a required complete-split transaction.
    missed_mocro_frame,_: 2;
    
    /// The controller sets this bit when an invalid response is received from the device.
    /// Causes may be timeout, CRC, bad PID etc.
    transaction_error,_: 3;
    
    /// This is set when the the controller detected babble during the transaction.
    /// Babble is when the controller receives more data from then endpoint than was expected.
    /// When this bit is set the [Self::halted] is also set.  
    babbble_detected,_: 4;
    
    /// The controller sets this bit when the controller is unable to transmit or receive data at 
    /// the rate required.
    data_buffer_err,_: 5;
    
    /// This bit is set to indicate a serious error has occurred at teh device/endpoint. 
    /// This may be caused by [Self::babble], the error counter reaching 0, or reception of the 
    /// stall handshake from the device. Any time that a transaction results in the halted bit
    /// being set the [Self::active] bit is also cleared.
    halted,_: 6;
    
    /// The active bit is set by software to enable execution of transactions defined by this
    /// [QueueElementTransferDescriptor].
    active, set_active: 7;
    
    /// Indicates the token used for transactions used with this descriptor.
    from into PidCode, get_pid_code,set_pid_code: 9,8;
    
    /// This field is copied from the qTD during the overlay and written back during the queue advancement.
    /// This field tracks the numebr of consecutive errors detected wile executing this 
    /// [QueueElementTransferDescriptor]. When this is set to a non-zero value the controller will 
    /// decrement the count and write it back. If the count is decremented to zero then the controller 
    /// will mark this QETD as inactive and the transaction fails with the halted bit set, and the 
    /// error-bit that caused the failure will be set. An interrupt will be raised if the
    /// [crate::operational_regs::IntEnable::USB_ERROR_INT] is set.
    ///
    /// If this field is set to zero then the controller will not count bus errors and will not 
    /// limit the number of retries for  the transaction.
    ///
    /// Note on an error the value is written back to the [QueueHead] not the [QueueElementTransferDescriptor]
    error_count, _: 11,10;
    
    /// This field indicates the buffer to use.
    /// 
    /// Valid values are `0..=4`
    /// 
    /// This field is not written back to by the controller.
    current_page, set_current_page: 14,12;
    
    
    /// When this bit is set the controller will raise an interrupt when this 
    /// [QueueElementTransferDescriptor] has completed execution.
    get_interrupt_on_complete, interrupt_on_complete: 15;
    
    /// This indicates the number of bytes to be moved for this [QueueElementTransferDescriptor].
    /// The controller will decrement this by the number of bytes actually transferred during this 
    /// transaction on successful completion.
    /// 
    /// Valid values are `0x0..=0x5000`
    /// 
    /// When this is zero the controller will execute a zero length transaction.
    /// 
    /// This field is not required to be a multiple of the maximum packet length.
    get_expected_size, expected_size: 30,16;
    
    /// This is the current value of the data toggle bit.
    ///
    /// [Ctl0::get_data_toggle_ctl] indicates whether this bit will be used.
    data_toggle, _: 31;
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum PidCode {
    Out = 0,
    In,
    Setup,
}

impl From<u32> for PidCode {
    fn from(value: u32) -> Self {
        match value {
            0 => PidCode::Out,
            1 => PidCode::In,
            2 => PidCode::Setup,
            _ => panic!("invalid value or PidCode"),
        }
    }
}

impl From<PidCode> for u32 {
    fn from(value: PidCode) -> Self {
        value as u32
    }
}

impl From<TransactionDirection> for PidCode {
    fn from(value: TransactionDirection) -> Self {
        match value {
            TransactionDirection::In => PidCode::In,
            TransactionDirection::Out => PidCode::Out,
        }
    }
}

trait BufferPointerRegister {
    fn bits(&self) -> u32;
    fn set_bits(&mut self, bits: u32);
    
    /// Returns the pointer of the buffer field.
    fn get_buffer(&self) -> u32 {
        self.bits() & !((4096)-1)
    }
    
    /// Sets the buffer pointer to `buffer`
    ///
    /// # Panics
    /// 
    /// `buffer` must have an alignment of `0x1000` 
    fn set_buffer(&mut self, buffer: u32) {
        assert_eq!(buffer & (4096-1), 0, "Buffer misaligned");
        let buzz_cut = self.get_buffer() & (4096-1);
        self.set_buffer(buzz_cut | buffer);
    }
    
}


/// This field contains the offset in bytes into the current register.
/// 
/// The top section of this buffer contains the base address of the first page in the transfer descriptor.
struct BufferOffset(u32);
impl BufferOffset {
    const fn new() -> Self {
        Self(0)
    }
    
    /// This sets the offset into the current buffer
    /// 
    /// # Panics
    /// 
    /// This fn will panic if `offset > 0x1000`. 
    fn set_offset(&mut self, offset: u32) {
        assert!(offset > 0x1000);
        let trimmed = self.0 & !(4096-1);
        self.0 = trimmed | offset;
    }
    
    /// Returns the offset into the buffer.
    fn get_offset(&self) -> u32 {
        self.0 & (4096-1)
    }
}

impl BufferPointerRegister for BufferOffset {
    fn bits(&self) -> u32 {
        self.0
    }

    fn set_bits(&mut self, bits: u32) {
        self.0 = bits
    }
}

struct BufferField(u32);

impl BufferPointerRegister for BufferField {
    fn bits(&self) -> u32 {
        self.0
    }

    fn set_bits(&mut self, bits: u32) {
        self.0 = bits
    }
}