
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
    /// This must always be present when [crate::_libboot_entry] is called.
    /// This is an option so that [Option::take] can be used.
    pub memory_map: Option<MemoryMap>,
    pub optionals: BootInfoOptionals
}

impl BootInfo {

    /// This doesn't work correctly. It should locate the PT_TLS segment from the ELF program headers,
    /// however I cannot figure out how to locate the program headers.
    ///
    /// Instead, it locates the sections called ".tdata" and ".tbss". Only one of these are required
    /// for this to return `Some(_)`
    /// If no ".tdata" section is present then the `file` field will be an empty slice
    pub fn get_tls_template(&self) -> Option<TlsTemplate> {
        let sections = self.optionals.mb2_info.as_ref()?.elf_sections()?;
        let mut len = 0;
        let mut init = None;
        for s in sections {
            if let Ok(n) = s.name() {
                match n {
                    ".tdata" => {
                        // SAFETY: This address and size is given by the bootloader
                        init = Some( unsafe { core::slice::from_raw_parts(s.start_address() as usize as *const u8, s.size() as usize) } );
                        len += s.size() as usize
                    }
                    ".tbss" => {
                        len += s.size() as usize;
                    }
                    _ => {}
                }
            }
        }

        if len == 0 {
            None
        } else {
            Some(TlsTemplate {
                file: init.unwrap_or( &[]),
                size: len,
            })
        }
    }

    /// Returns the RDP address
    pub fn rsdp_ptr(&self) -> Option<Rsdp> {
        // We get the actual pointer to the RSDP, we must increment the ptr by 8 because there is an MBI header there
        if let Some(ref mb2) = self.optionals.mb2_info {
            if let Some(r) = mb2.rsdp_v2_tag() {
                Some(Rsdp::RsdpV2(r as *const _ as usize + 8))
            } else if let Some(r) = mb2.rsdp_v1_tag() {
                Some(Rsdp::RsdpV1(r as *const _ as usize + 8))
            } else {
                None
            }
        } else { None } // more to come
    }
}

pub enum Rsdp {
    RsdpV1(usize),
    RsdpV2(usize),
}

impl Rsdp {
    pub fn addr(&self) -> usize {
        match self {
            Rsdp::RsdpV1(p) => *p,
            Rsdp::RsdpV2(p) => *p,
        }
    }
}

pub struct TlsTemplate {
    /// A raw slice containing the initialized TLS data.
    /// It is guaranteed to be valid when to valid memory when [crate::_libboot_entry] is called.
    pub file: &'static [u8],
    /// Total size of the thread local segment.
    /// During initialization this much memory should be allocated.
    pub size: usize,
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
    pub framebuffer: &'static mut [u8]
}

pub enum PixelFormat {
    /// Big endian RGB
    Rgb32,
    /// Little endian RGB
    Bgr32,
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

impl MemoryMap {
    pub fn iter(&self) -> MapIter {
        match self {
            #[cfg(feature = "uefi")]
            Self::Uefi(u) => MapIter::Uefi(u.entries()),
        }
    }

    /// Modifies the memory map to ensure that each region is page aligned.
    pub fn sanitize(&mut self) {
        match self {
            #[cfg(feature = "uefi")]
            MemoryMap::Uefi(_) => {} // already sanitized
        }
    }
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
        let ty = match value.ty {
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
            ty,
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

impl PartialEq for MemoryRegionType {
    fn eq(&self, other: &Self) -> bool {
        core::mem::discriminant(self) == core::mem::discriminant(other)
    }
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