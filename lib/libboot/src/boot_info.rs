
#[cfg(feature = "uefi")]
pub use ::uefi::table::boot::MemoryType;
#[cfg(feature = "uefi")]
pub use ::uefi::table::boot::MemoryAttribute;

use cfg_if::cfg_if;

cfg_if! {
    if #[cfg(any(target_arch = "x86",target_arch = "x86_64"))] {
        const PAGE_SIZE: usize = 4096;
    } else {
        compiler_error!("Page size unknown for arch")
    }
}

pub struct BootInfo {
    /// Address of the physical address offset.
    /// This is always a [u64] even on 32bit systems.
    /// For 32bit systems this may be configured to always be a 32bit offset
    /// If memory is identity mapped then this is 0
    pub physical_address_offset: u64,
    pub memory_map: MemoryMap,
    pub optionals: BootInfoOptionals
}

#[derive(Default)]
#[non_exhaustive]
pub struct BootInfoOptionals {
    #[cfg(feature = "multiboot2")]
    pub mb2_info: Option<multiboot2::BootInformation<'static>>,
    #[cfg(feature = "uefi")]
    pub efi_system_table: Option<uefi::table::SystemTable<uefi::table::Runtime>>,
    pub graphic_info: Option<GraphicInfo>,
}


/// Contains information required to operate the framebuffer.
///
/// The width and height fields represent the resolution of the display in pixels.
/// The stride is the number of pixels per scan line, stride will never be less than width.
///
/// ``` |            width            | stride | ```
///
/// The stride region of a scan line will not be displayed.
pub struct GraphicInfo {
    pub width: u64,
    pub height: u64,
    pub stride: u64,
    pub pixel_format: PixelFormat,
    pub framebuffer: &'static [u8]
}

pub enum PixelFormat {
    /// Big endian RGB
    Rgb,
    /// Little endian RGB
    Bgr,
    /// Video hardware is using a custom pixel format.
    /// Masks for each channel are given below.
    /// `reserved` contains bits which are ignored.
    /// The `xor` and `or` of all fields bits should be [u32::MAX]
    ColourMask {
        red: u32,
        green: u32,
        blue: u32,
        reserved: u32
    }
}

#[cfg(feature = "uefi")]
pub type UefiMemoryMap = uefi::table::boot::MemoryMap<'static>;

#[non_exhaustive]
pub enum MemoryMap {
    #[cfg(feature = "uefi")]
    Uefi(UefiMemoryMap)
}

#[non_exhaustive]
pub enum MapIter<'a> {
    #[cfg(feature = "uefi")]
    Uefi(uefi::table::boot::MemoryMapIter<'a>)
}

#[derive(Debug)]
pub struct MemoryRegion {
    pub phys_addr: u64,
    pub size: u64,
    pub ty: MemoryRegionType,
    pub distinct: MemoryRegionDistinct
}

#[cfg(feature = "uefi")]
impl From<uefi::table::boot::MemoryDescriptor> for MemoryRegion {
    fn from(value: uefi::table::boot::MemoryDescriptor) -> Self {
        match value.ty {
            MemoryType::LOADER_DATA => MemoryRegionType::Bootloader,

            MemoryType::RESERVED |
            MemoryType::UNUSABLE |
            MemoryType::MMIO |
            MemoryType::RUNTIME_SERVICES_CODE |
            MemoryType::RUNTIME_SERVICES_DATA |
            MemoryType::ACPI_RECLAIM |
            MemoryType::ACPI_NON_VOLATILE |
            MemoryType::PAL_CODE |
            MemoryType::MMIO_PORT_SPACE => MemoryRegionType::Unusable(value.ty.0),

            MemoryType::CONVENTIONAL |
            MemoryType::BOOT_SERVICES_DATA |
            MemoryType::BOOT_SERVICES_CODE |
            MemoryType::LOADER_CODE => MemoryRegionType::Usable,

            e => MemoryRegionType::Unknown(e.0)
        };

        Self {
            phys_addr: value.phys_start,
            size: value.page_count * PAGE_SIZE as u64,
            ty: MemoryRegionType::Usable,
            distinct: MemoryRegionDistinct::Uefi {
                ty: value.ty,
                virt_addr: value.virt_start,
                attribute: value.att
            },
        }
    }
}

#[derive(Debug)]
pub enum MemoryRegionType {
    /// Free memory usable by the kernel
    Usable,
    /// Memory used by the bootloader, this contains any information structures that may have been
    /// passed to the kernel.
    /// This may also include the kernel itself.
    Bootloader,
    /// Unusable memory.
    /// This contains the backing type for this region.
    /// To determine the type of this region check the [MemoryRegionDistinct] which was given
    /// with [MemoryRegion] this is contained in.
    ///
    /// Some regions with this type may be accessed but cannot be used freely either because it may
    /// have read or write side effects.
    ///
    /// Some memory with this type may be reclaimed at runtime
    Unusable(u32),
    /// Unknown memory type
    /// See [MemoryRegionType::Unusable] for info on how to determine the actual type.
    Unknown(u32),
}

#[derive(Debug)]
#[non_exhaustive]
pub enum MemoryRegionDistinct {
    #[cfg(feature = "uefi")]
    Uefi {
        ty: MemoryType,
        virt_addr: u64,
        attribute: MemoryAttribute,
    }
}

impl<'a> Iterator for MapIter<'a> {
    type Item = MemoryRegion;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            #[cfg(feature = "uefi")]
            MapIter::Uefi(i) => {
                Some(i.next()?.clone().into())
            },
        }
    }
}