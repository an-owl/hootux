//! Queries explain to the consumer how to interperate the output data.
//! The data format should be simple but variable. For example [crate::keyboard] describes a format
//! where a key is given and a key state. The key value is not variable whereas the state is.
//! Hall effect and optical keyboards can track a keys position as a state greater than open or closed.

#[repr(u8)]
pub enum DataType {
    /// Booleans can be packed.
    Boolean(u64) = 0x00,
    Integer(IntSize) = 0x01,
    Float(IntSize) = 0x02,
}

#[repr(u8)]
pub enum IntSize {
    Bits8,
    Bits16,
    Bits32,
    Bits64,
}

#[repr(u8)]
pub enum FloatSize {
    SinglePrecision,
    DoublePrecision,
}
