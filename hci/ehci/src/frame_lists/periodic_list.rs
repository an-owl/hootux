use bitfield::bitfield;
use num_enum::TryFromPrimitive;


#[repr(C)]
struct PeriodicFrameList {
    list: [FrameListLinkPointer;1024]
}


bitfield! {
    struct FrameListLinkPointer(u32);
    impl Debug;

    /// When set this bit indicates that indicates that [Self::ptr] is invalid
    end,set_end: 0;
    from into FrameListLinkType, get_link_type, set_link_type: 2,1;
    ptr,set_ptr: 31,5;
}

impl FrameListLinkPointer {
    fn new(next_addr: Option<(u32,FrameListLinkType)>) -> Self {
        let mut this = Self(0);
        if let Some((ptr,ty)) = next_addr {
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

enum FrameListNode {
    IsochronousTransferDescriptor(isochronous_transfer::IsochronousTransferDescriptor),
    QueueHead(),
    SplitTransactionIsochronousTransferDescriptor(),
    FrameSpanTraversalNode(),
}

pub mod isochronous_transfer {
    use core::marker::PhantomData;
    use core::mem::{offset_of, MaybeUninit};
    use bitfield::bitfield;
    use num_enum::{IntoPrimitive, TryFromPrimitive};
    use crate::frame_lists::Target;
    use super::{FrameListLinkPointer,FrameListLinkType};

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
        int: bool
    }

    impl ItdTransactionInfo {
        const fn new(buffer: u8, offset: u16, len: Option<u16>,interrupt: bool) -> Self {
            assert!(buffer < 8);
            assert!(offset < 1024);
            Self { buffer, offset, length: len, int: interrupt }
        }
    }

    impl IsochronousTransferDescriptor {

        /// Constructs a new IsochronousTransferDescriptor.
        ///
        /// `transfers` is an iterator which describes each transfer operation.
        /// Note that when `direction` is [super::super::TransactionDirection::Out] then the length
        /// of all transactions must be specified. Specified buffers must be described by `buffers` 
        /// and the length+offest must not exceed the specified length of the buffer.
        ///
        /// `buffers` is similar to `direction` where it describes the transfer buffers as a range of 64bit addresses.
        /// 
        /// # Panics
        /// 
        /// This fn will panic if any of the following conditions are met. 
        /// 
        /// * `transfers` yields more than 8 descriptors
        /// * `buffers` yields more than 7 buffers.
        /// * Any of the buffers specified by `transfers` wasn't defined
        /// * A transaction descriptor attempts to overflow its target buffer. 
        /// 
        /// # Safety
        /// 
        /// The caller must ensure that all buffers yielded by `buffers` must not specify memory 
        /// which is in use and that the buffer is safe to use for DMA.
        /// See [Embedonomicon](https://docs.rust-embedded.org/embedonomicon/dma.html) for info about DMA safety.
        // note that this uses u64 as an address because these are physical pointers.
        pub unsafe fn new(next: Option<(u32, FrameListLinkType)>,
               target: Target ,
               transfers: impl Iterator<Item=ItdTransactionInfo>,
               buffers: impl Iterator<Item=core::ops::Range<u64>>,
               direction: super::super::TransactionDirection
        ) -> Self {
            let mut this = Self {
                next_node: FrameListLinkPointer::new(next),
                descriptors: [IsochronousTransactionDescriptor::empty(); 8],
                buff_addr: BufferPointerField::empty(),
                buff_packet_desc: BufferPointerField::empty(),
                buff_multi: BufferPointerField::empty(),
                buffers_3_6: [BufferPointerField::empty();3]
            };

            this.buff_packet_desc.set_direction(direction);
            this.buff_addr.set_tgt(target);

            let mut present_buffers: [Option<core::ops::Range<u64>>;7] = [const { None }; 7];
            for (i,buff) in buffers.enumerate() {
                
                *present_buffers.get_mut(i).expect("Excessive buffer descriptors given") = Some(buff.clone());
                unsafe { this.set_buffer(i as u8, buff.start) };
            }
            
            for (i,transaction) in transfers.enumerate() {
                if i >= 8 {
                    panic!("Excessive transaction descriptors given");
                }
                let ItdTransactionInfo {
                    buffer,
                    offset,
                    length,
                    int,
                } = transaction;
                
                let t = present_buffers[buffer as usize].as_ref().unwrap_or_else(|| panic!("Transaction descriptor {i} specified buffer {buffer} which was not defined")); //.expect but with lazy format-str
                let t_len = t.end - t.start;
                let tgt_len = (length.unwrap_or(0) as u64) + (offset as u64); 
                if tgt_len > t_len {
                    panic!("Transaction descriptor {i} specified len {tgt_len:#x} which exceeds buffer {buffer}: {t:?}-{t_len:#x}");
                }
                unsafe { this.set_transaction(i as u8, transaction, true) };
                this.descriptors[i].interrupt_on_complete(int)
            };
            this
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
                _ => unreachable!()
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
            let arr_ptr = unsafe { self.as_ptr().byte_add(offset_of!(IsochronousTransferDescriptor,descriptors)).cast::<IsochronousTransactionDescriptor>() };
            // SAFETY: Wee assert that descriptor is a valid value above
            let tgt_ptr = unsafe { arr_ptr.offset(descriptor as usize as isize) }; // extra step to explicitly disallow sign extension

            unsafe { tgt_ptr.read_volatile() }
        }

        fn get_buffer_ptr(&self, buffer: u8) -> u64 {
            assert!(buffer < 6);

            // SAFETY: Offset is fetched from core::mem::offset_of which guarantees we get the correct offset.
            // This may technically read an illegal invariant, but all BufferPointerField<T>'s buffer address can be safely accessed via BufferPointerField
            let arr_ptr = unsafe { self.as_ptr().byte_add(offset_of!(IsochronousTransferDescriptor,buff_addr)).cast::<BufferPointerField>() };

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
                address: super::super::Address::new(addr as u8),
                endpoint: super::super::Endpoint::new(endpoint as u8),
            }
        }

        /// Sets the target function and endpoint.
        fn set_tgt(&mut self,target: Target) {
            

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

        fn set_direction(&mut self, direction: super::super::TransactionDirection) {

            if direction.into() {
                self.inner |= Self::DIRECTION_BIT;
            } else {
                self.inner &= !Self::DIRECTION_BIT;
            }
        }

        fn get_direction(&self) -> super::super::TransactionDirection {
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

    #[repr(u8)]
    #[derive(Debug, Copy, Clone, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
    enum TransactionCount {
        One = 1,
        Two,
        Three,
    }
}