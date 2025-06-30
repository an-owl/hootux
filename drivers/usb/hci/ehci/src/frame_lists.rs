use bitfield::bitfield;
use num_enum::{IntoPrimitive, TryFromPrimitive};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TransactionDirection {
    Out,
    In,
}

impl From<bool> for TransactionDirection {
    fn from(value: bool) -> Self {
        match value {
            true => Self::In,
            false => Self::Out,
        }
    }
}

impl From<TransactionDirection> for bool {
    fn from(value: TransactionDirection) -> Self {
        match value {
            TransactionDirection::In => true,
            TransactionDirection::Out => false,
        }
    }
}

pub struct Target {
    pub address: Address,
    pub endpoint: Endpoint,
}

/// Each USB function contains 16 possible endpoints.
/// Endpoint 0 is reserved as the control endpoint, the remaining 15 can be configured depending
/// on the function of the device.
pub struct Endpoint(u8);

impl Endpoint {
    pub const CONTROL: Self = Self(0);

    /// Constructs a new `self`.
    ///
    /// # Panics
    ///
    /// This fn will panic if `endpoint >= 16`
    pub const fn new(endpoint: u8) -> Self {
        assert!(endpoint < 16);
        Self(endpoint)
    }
}

impl From<Endpoint> for u8 {
    fn from(endpoint: Endpoint) -> Self {
        endpoint.0
    }
}

/// Each USB function must be configured by the host with an address ranging from 1 to 63.
/// This struct represents possible addresses.
/// Address `0` is reserved for broadcasts where all functions are required to respond and cannot
/// be assigned to a function.
pub struct Address(u8);

impl Address {
    pub const BROADCAST: Self = Self(0);

    /// Constructs a new Address.
    ///
    /// # Panics
    ///
    /// This fn will panic if `address >= 64`
    pub const fn new(address: u8) -> Self {
        assert!(address < 64);
        Self(address)
    }
}

impl From<Address> for u8 {
    fn from(address: Address) -> Self {
        address.0
    }
}

bitfield::bitfield! {
    struct FrameListLinkPointer(u32);
    impl Debug;

    /// When set this bit indicates that indicates that [Self::ptr] is invalid
    end,set_end: 0;
    from into FrameListLinkType, get_link_type, set_link_type: 2,1;
    // fixme i think this has a bug where the data << 5 when it should not be
    ptr,set_ptr: 31,5;
}

impl FrameListLinkPointer {
    fn new(next_addr: Option<(u32, FrameListLinkType)>) -> Self {
        let mut this = Self(0);
        if let Some((ptr, ty)) = next_addr {
            this.set_ptr(ptr);
            this.set_link_type(ty);
            this.set_end(false);
        } else {
            this.set_end(true);
        }

        this
    }
}
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, TryFromPrimitive)]
pub enum FrameListLinkType {
    IsochronousTransferDescriptor = 0,
    QueueHead,
    SplitTransactionIsochronousTransferDescriptor,
    FrameSpanTraversalNode,
}

impl Into<u32> for FrameListLinkType {
    fn into(self) -> u32 {
        self as u8 as u32
    }
}

impl From<u32> for FrameListLinkType {
    fn from(value: u32) -> Self {
        value.try_into().unwrap()
    }
}

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
pub enum TransactionCount {
    One = 1,
    Two,
    Three,
}

impl From<u32> for TransactionCount {
    fn from(value: u32) -> Self {
        value.try_into().unwrap()
    }
}

impl From<TransactionCount> for u32 {
    fn from(value: TransactionCount) -> Self {
        value as u32
    }
}

#[repr(C)]
pub struct QueueHead {
    next_link_ptr: FrameListLinkPointer,
    ctl0: Ctl0,
    ctl1: Ctl1,
    /// The lower 32bits of the address of the transaction descriptor.
    /// This must be aligned to 32.
    current_transaction: FrameListLinkPointer,
    next_pointer: FrameListLinkPointer,
    alternate_pointer: FrameListLinkPointerWithNakCount,
    overlay: [u32; 6],
}

impl QueueHead {
    pub fn new() -> Self {
        Self {
            next_link_ptr: FrameListLinkPointer::new(None),
            ctl0: Ctl0(0),
            ctl1: Ctl1(0),
            current_transaction: FrameListLinkPointer::new(None),
            next_pointer: FrameListLinkPointer::new(None),
            alternate_pointer: FrameListLinkPointerWithNakCount(0),
            overlay: [0; 6],
        }
    }

    /// Sets the current transaction pointer to `addr`
    ///
    /// # Panics
    ///
    /// This fn will panic if `addr` is not aligned to `32`
    ///
    /// # Safety
    ///
    /// The caller must ensure that `addr` contains the address of a properly terminated [QueueElementTransferDescriptor]
    pub unsafe fn set_current_transaction(&mut self, addr: u32) {
        assert_eq!(addr & (0x32 - 1), 0);
        self.current_transaction.set_ptr(addr);
    }

    /// Sets the "H bit" indicating this is the head of the reclamation list.
    /// Only one queue head in the asynchronous schedule should have the "H bit" set.
    pub fn set_head_of_list(&mut self) {
        self.ctl0.set_head_reclimation_list(true)
    }

    pub fn set_target(&mut self, target: Target) {
        self.ctl0.set_addr(target.address.0 as u32);
        self.ctl0.set_enpoint(target.endpoint.0 as u32);
    }

    pub fn set_next_queue_head(&mut self, addr: u32) {
        self.next_link_ptr.set_ptr(addr);
    }
}

bitfield! {
    struct Ctl0(u32);
    impl Debug;

    /// See [Address]
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

    endpoint,set_enpoint: 11,8;

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
pub enum EndpointSpeed {
    FullSpeed = 0,
    LowSpeed = 1,
    HighSpeed = 2,
}

impl From<u32> for EndpointSpeed {
    fn from(value: u32) -> Self {
        (value as u8)
            .try_into()
            .expect("invalid value or endpoint speed")
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
    /// The lower 3 bits of [operational_regs::FrameIndex] are a index into this field.
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
    u8, into Address, _ ,set_bridge_hub: 22,16;

    /// When this queue head indicates a split transaction this field indicates the port number of
    /// the bridge-hub that the target is attached to.
    u8, _, set_port_number: 19,23;

    /// Indicates the number of successive transactions the controller may send to this endpoint per
    /// microframe.
    // fixme: I think. Double check this.
    from into TransactionCount, get_endpoint_transaction_count, set_endpoint_transaction_count: 31,30;
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

impl FrameListLinkPointerWithNakCount {
    fn new() -> Self {
        let mut this = Self(0);
        this.set_end(true);
        this
    }
}

#[repr(C, align(32))]
pub struct QueueElementTransferDescriptor {
    next_qtd: FrameListLinkPointer,
    /// This field points to the qTD that is used when in the event that the current qTD execution
    /// encounters a short packet for an [TransactionDirection::In] transaction.
    alt_qtd: FrameListLinkPointer,
    config: QtdConfig,
    buffer_offset: BufferOffset,
    buffers: [BufferField; 4],
}

impl QueueElementTransferDescriptor {
    pub fn new() -> Self {
        Self {
            next_qtd: FrameListLinkPointer::new(None),
            alt_qtd: FrameListLinkPointer::new(None),
            config: QtdConfig(0),
            buffer_offset: BufferOffset(0),
            buffers: [const { BufferField(0) }; 4],
        }
    }

    /// Sets the next and alternate QTD fields in `self`.
    ///
    /// When either `next` is `None` the terminate bit will be set disabling traversal
    /// to the address in that field.
    ///
    /// # Safety
    ///
    /// The caller must ensure that when `next` and `alternate` are `Some(_)` that they contain
    /// valid pointers to another `QTD`
    pub unsafe fn set_next(&mut self, next: Option<u32>) {
        if let Some(next) = next {
            self.next_qtd.set_ptr(next);
            self.next_qtd.set_end(false); // Clear T-bit after setting address to prevent wild pointer dereferencing.
        } else {
            self.next_qtd.set_end(true);
        }
    }

    /// Performs the same operation as [Self::set_next] on the alternate pointer.
    pub unsafe fn set_alternate(&mut self, alternate: Option<u32>) {
        if let Some(next) = alternate {
            self.alt_qtd.set_ptr(next);
            self.alt_qtd.set_end(false);
        } else {
            self.alt_qtd.set_end(true);
        }
    }
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
pub enum PidCode {
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
        self.bits() & !((4096) - 1)
    }

    /// Sets the buffer pointer to `buffer`
    ///
    /// # Panics
    ///
    /// `buffer` must have an alignment of `0x1000`
    fn set_buffer(&mut self, buffer: u32) {
        assert_eq!(buffer & (4096 - 1), 0, "Buffer misaligned");
        let buzz_cut = self.get_buffer() & (4096 - 1);
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
        let trimmed = self.0 & !(4096 - 1);
        self.0 = trimmed | offset;
    }

    /// Returns the offset into the buffer.
    fn get_offset(&self) -> u32 {
        self.0 & (4096 - 1)
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

#[repr(C)]
pub struct PeriodicFrameList {
    list: [FrameListLinkPointer; 1024],
}

pub enum FrameListNode {
    IsochronousTransferDescriptor(IsochronousTransferDescriptor),
    QueueHead(),
    SplitTransactionIsochronousTransferDescriptor(),
    FrameSpanTraversalNode(),
}

use core::marker::PhantomData;
use core::mem::{MaybeUninit, offset_of};

/// When the controller executes a `IsochronousTransferDescriptor` bits 2:0 are used to select
/// the targeted transaction. During the transaction if the buffer targeted by the transaction
/// overflows the controller will use the following buffer.
///
/// The maximum payload for an isochronous transaction is `1024` bytes however multiple
/// transactions may be executed [MultiTransactionField] times, potentially returning 3072 bytes
/// in a single μframe.
///
/// Allowing buffer `6` to overflow is UB.
#[repr(C, align(32))]
pub struct IsochronousTransferDescriptor {
    next_node: FrameListLinkPointer,
    descriptors: [IsochronousTransactionDescriptor; 8],
    buff_addr: BufferPointerField<AddressLower>,
    buff_packet_desc: BufferPointerField<TransactionDescription>,
    buff_multi: BufferPointerField<MultiTransactionField>,
    buffers_3_6: [BufferPointerField; 3],
}

pub struct ItdTransactionInfo {
    buffer: u8,
    offset: u16,
    length: Option<u16>,
    int: bool,
}

impl ItdTransactionInfo {
    const fn new(buffer: u8, offset: u16, len: Option<u16>, interrupt: bool) -> Self {
        assert!(buffer < 8);
        assert!(offset < 1024);
        Self {
            buffer,
            offset,
            length: len,
            int: interrupt,
        }
    }
}

impl IsochronousTransferDescriptor {
    /// Constructs a new IsochronousTransferDescriptor.
    ///
    /// `transfers` is an iterator which describes each transfer operation.
    /// Note that when `direction` is [TransactionDirection::Out] then the length
    /// of all transactions must be specified. Specified buffers must be described by `buffers`
    /// and the length+offest must not exceed the specified length of the buffer.
    ///
    /// `buffers` is similar to `direction` where it describes the transfer buffers as a range of 64bit addresses.
    pub unsafe fn new(
        next: Option<(u32, FrameListLinkType)>,
        target: Target,
        transfers: impl Iterator<Item = ItdTransactionInfo>,
        buffers: impl Iterator<Item = core::ops::Range<u64>>,
        direction: TransactionDirection,
    ) -> Self {
        Self {
            next_node: FrameListLinkPointer::new(next),
            descriptors: [IsochronousTransactionDescriptor::empty(); 8],
            buff_addr: BufferPointerField::empty(),
            buff_packet_desc: BufferPointerField::empty(),
            buff_multi: BufferPointerField::empty(),
            buffers_3_6: [BufferPointerField::empty(); 3],
        }
    }

    /// Sets the given buffer to `addr`
    ///
    /// # Panics
    ///
    /// `buffer` must be lower than 8 and `addr` must be a valid size for `self` and aligned to `4096`
    ///
    /// # Safety
    ///
    /// The caller must ensure that the allocated buffer is large enough to accommodate the maximum
    /// packet size and will not be overrun.
    unsafe fn set_buffer(&mut self, buffer: u8, addr: u64) {
        assert!(buffer < 8);
        assert!(addr <= u32::MAX as u64);
        match buffer {
            0 => self.buff_addr.set_pointer(addr as u32),
            1 => self.buff_packet_desc.set_pointer(addr as u32),
            2 => self.buff_multi.set_pointer(addr as u32),
            3 | 4 | 5 => self.buffers_3_6[(buffer - 3) as usize].set_pointer(addr as u32),
            _ => unreachable!(),
        }
    }

    /// Configures the transaction indicated by `transaction`.
    ///
    /// # Panics
    ///
    /// This fn will panic if `transaction >= 8`.
    ///
    /// # Safety
    ///
    /// When `pending == true` the caller must ensure that the offset + length of the transaction does not overflow the target buffer.
    // todo improve this or wrap it
    unsafe fn set_transaction(&mut self, transaction: u8, desc: ItdTransactionInfo, pending: bool) {
        assert!(transaction < 8);
        let ItdTransactionInfo {
            buffer,
            offset,
            length,
            int,
        } = desc;

        let td = &mut self.descriptors[transaction as usize];
        td.set_page(buffer as u32);
        td.set_transaction_offset(offset as u32);
        td.set_transaction_length(length.unwrap_or(0) as u32);
        td.interrupt_on_complete(int);
        td.set_pending(pending);
    }
}

trait ItdExt {
    fn get_transfer_descriptor(&self, descriptor: u8) -> IsochronousTransactionDescriptor;

    // u64 to allow 64bit extension
    fn get_buffer_ptr(&self, buffer: u8) -> u64;
}

impl ItdExt for MaybeUninit<IsochronousTransferDescriptor> {
    fn get_transfer_descriptor(&self, descriptor: u8) -> IsochronousTransactionDescriptor {
        assert!(descriptor < 8);
        // SAFETY: Offset is fetched from core::mem::offset_of which guarantees we get the correct offset.
        // ptr points to the first IsochronousTransactionDescriptor
        let arr_ptr = unsafe {
            self.as_ptr()
                .byte_add(offset_of!(IsochronousTransferDescriptor, descriptors))
                .cast::<IsochronousTransactionDescriptor>()
        };
        // SAFETY: Wee assert that descriptor is a valid value above
        let tgt_ptr = unsafe { arr_ptr.offset(descriptor as usize as isize) }; // extra step to explicitly disallow sign extension

        unsafe { tgt_ptr.read_volatile() }
    }

    fn get_buffer_ptr(&self, buffer: u8) -> u64 {
        assert!(buffer < 6);

        // SAFETY: Offset is fetched from core::mem::offset_of which guarantees we get the correct offset.
        // This may technically read an illegal invariant, but all BufferPointerField<T>'s buffer address can be safely accessed via BufferPointerField
        let arr_ptr = unsafe {
            self.as_ptr()
                .byte_add(offset_of!(IsochronousTransferDescriptor, buff_addr))
                .cast::<BufferPointerField>()
        };

        // SAFETY: arr_ptr is guaranteed to be aligned
        unsafe { arr_ptr.offset(buffer as usize as isize).read_volatile() }.get_pointer() as u64
    }
}

bitfield! {
    #[repr(transparent)]
    #[derive(Copy, Clone)]
    struct IsochronousTransactionDescriptor(u32);

    /// Indicates the offset into the selected page this transaction should use.
    // So we can use a single page multiple times.
    transaction_offset, set_transaction_offset: 11,0;

    /// Indicates which page in [IsochronousTransferDescriptor] will be used by this transaction.
    ///
    /// Only values between `0..=6` are valid.
    selected_page, set_page: 14,12;

    /// When set the controller will raise an interrupt at the next interrupt threshold.
    _ , interrupt_on_complete: 15;

    /// For an OUT transaction this indicates the number of bytes that will be send by the controller.
    /// The controller is not required to update this field to indicate the number of bytes transmitted.
    // So it might
    ///
    /// For an IN transaction this indicates the number of bytes software expects to be sent.
    /// During the status update the controller writes back the number of bytes successfully delivered.
    transaction_length, set_transaction_length : 27,16;

    /// Set by the controller when an invalid response is received during an IN transaction
    transaction_err, _ : 28;

    /// Set by the controller when babble is detected.
    /// Babble is when more data was send by the device than was expected.
    babble, _: 29;

    /// Indicates that the controller uis unable to keep up with reception of data or
    /// or is unable to send data fast enough.
    buffer_error, _ : 30;

    /// Software must set this to `true` to enable the execution of this
    /// transaction.
    /// This is set to `false` by the controller when its transaction is completed.
    is_pending, set_pending: 31;
}

impl IsochronousTransactionDescriptor {
    /// Returns an empty Self. This contains no data and will not be executed.
    const fn empty() -> Self {
        Self(0)
    }
}

/// Marker trait for [BufferPointerField]s to indicate what is stored in the lower 12 bits.
trait BufferPointerLower: Copy + Clone {}

/// For fields 2:6
#[derive(Copy, Clone)]
struct ReservedLower;
impl BufferPointerLower for ReservedLower {}

#[repr(transparent)]
#[derive(Copy, Clone)]
struct BufferPointerField<L: BufferPointerLower = ReservedLower> {
    inner: u32,
    phantom_data: PhantomData<L>,
}

impl<L: BufferPointerLower> BufferPointerField<L> {
    const fn empty() -> Self {
        Self {
            inner: 0,
            phantom_data: PhantomData,
        }
    }

    // Note: using the bitfield macro wont work here because it will left shift the value to lsb=0
    fn get_pointer(&self) -> u32 {
        self.inner & !(4096 - 1) // mask lower bits
    }

    /// Sets the pointer to `ptr`.
    ///
    /// # Panics
    ///
    /// This fn wil panic if `ptr` isn't aligned to 4K
    #[inline]
    fn set_pointer(&mut self, ptr: u32) {
        let mut data = self.inner;
        data &= 4096 - 1; // clear higher bits
        assert_eq!(ptr & !(4096 - 1), 0); // assert alignment of `ptr`
        data |= ptr;
        self.inner = data;
    }
}

/// For field 0
#[derive(Copy, Clone)]
struct AddressLower;
impl BufferPointerLower for AddressLower {}

impl BufferPointerField<AddressLower> {
    /// Returns the device address and the endpoint that this field describes
    fn get_tgt(&self) -> Target {
        let addr = self.inner & !((1 << 6) - 1);
        let endpoint = (self.inner >> 8) & !((1 << 4) - 1);
        Target {
            address: Address::new(addr as u8),
            endpoint: Endpoint::new(endpoint as u8),
        }
    }

    /// Sets the target function and endpoint.
    fn set_tgt(&mut self, target: Target) {
        let addr = (target.endpoint.0 as u32) << 8;
        let endpoint = (target.endpoint.0 as u32) << 8;
        let mut semi = self.inner;

        const MASK: u32 = !((!(1 << 6) - 1) << 8) | (!(1 << 4) - 1); // i may have added extra steps in this but idc

        // clear fields
        semi &= MASK;
        // set fields
        semi |= addr;
        semi |= endpoint;

        self.inner = semi;
    }
}

#[derive(Copy, Clone)]
struct TransactionDescription;
impl BufferPointerLower for TransactionDescription {}
impl BufferPointerField<TransactionDescription> {
    const DIRECTION_BIT: u32 = 1 << 11;

    /// Returns the maximum packet size allowed for this
    // todo make non-primitive
    fn get_packet_size(&self) -> u16 {
        (self.inner & ((1 << 10) - 1)) as u16
    }

    fn set_packet_size(&mut self, size: u16) {
        self.inner = size as u32;
    }

    fn set_direction(&mut self, direction: TransactionDirection) {
        if direction.into() {
            self.inner |= Self::DIRECTION_BIT;
        } else {
            self.inner &= !Self::DIRECTION_BIT;
        }
    }

    fn get_direction(&self) -> TransactionDirection {
        (self.inner & Self::DIRECTION_BIT != 0).into()
    }
}

#[derive(Copy, Clone)]
struct MultiTransactionField;
impl BufferPointerLower for MultiTransactionField {}

impl BufferPointerField<MultiTransactionField> {
    fn get_multi_transaction(&self) -> TransactionCount {
        ((self.inner & 3) as u8).try_into().unwrap() // `0` is an illegal invariant
    }
}
