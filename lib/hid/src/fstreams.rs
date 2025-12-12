//! Hootux HID streams are multiplexed, devices are represented as a single file, with multiple
//! interfaces defined for it.
//! Interfaces are devices such as keyboards, pointers, gamepads, etc. are split into multiple
//! streams within the same file.
//! Streams are accessed using the head position in the file, this module defines how to resolve
//! which streams are present and how to address each stream.

bitflags::bitflags! {

    /// HID device files may represent a multitude of different types of device, this struct defines how to access each stream.
    ///
    /// Bits between 0..31 are the selector field, and indicate which stream to access.
    /// By default, this field is a bitfield which allows multiple streams to be selected.
    /// When multiple streams are selected the first byte read will indicate which stream it belongs to.
    ///
    /// [Bit 63](Self::NO_BITFIELD) changes the selector field to use an index instead of a bitfield.
    /// This allows access to `0..u32::MAX` if multiple streams are required by the caller then the
    /// file may be read multiple times from the same file object or different file objects.
    ///
    /// [Bit 62](Self::QUERY) will query the interface, when this bit is set the file will output
    /// information regarding the specific format of the interface. The query format is defined by the interface.
    ///
    /// Bits `32..=62` (inclusive) are used as an argument into the interface.
    /// This must be set to `0` when multiple interfaces are selected.
    pub struct HidIndexFlags: u64 {
        /// Disables the bitfield, when this bit is set bits 0..31 are treated as an index.
        const NO_BITFIELD = 0x1 << 63;
        const QUERY = 0x1 << 62;

        const KEYBOARD = 0x1 << 0;
        /// Mouse, hamster etc.
        const RODENT = 0x1 << 1;
    }
}
