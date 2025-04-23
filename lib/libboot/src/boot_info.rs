#[cfg(feature = "uefi")]
pub use ::uefi::table::boot::MemoryAttribute;
#[cfg(feature = "uefi")]
pub use ::uefi::table::boot::MemoryType;
use cfg_if::cfg_if;
use multiboot2::MemoryAreaType;

cfg_if! {
    if #[cfg(any(target_arch = "x86",target_arch = "x86_64"))] {
        pub const PAGE_SIZE: usize = 4096;
    } else {
        compiler_error!("Page size unknown for arch")
    }
}

// ensure that the page size is less than one whole page
const _: () = {
    assert!(size_of::<BootInfo>() <= PAGE_SIZE);
};

pub struct BootInfo {
    /// Address of the physical address offset.
    /// This is always a [u64] even on 32bit systems.
    /// For 32bit systems this may be configured to always be a 32bit offset
    /// If memory is identity mapped then this is 0
    pub physical_address_offset: u64,
    /// This must always be present when [crate::_libboot_entry] is called.
    /// This is an option so that [Option::take] can be used.
    pub memory_map: Option<MemoryMap>,
    pub optionals: BootInfoOptionals,
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
                        init = Some(unsafe {
                            core::slice::from_raw_parts(
                                s.start_address() as usize as *const u8,
                                s.size() as usize,
                            )
                        });
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
                file: init.unwrap_or(&[]),
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
        } else {
            None
        } // more to come
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
    pub framebuffer: &'static mut [u8],
}

#[derive(Clone, Copy, Debug, PartialOrd, PartialEq)]
pub enum PixelFormat {
    /// Big endian RGB
    Rgb32,
    /// Big endian RGB with 24bits per pixel
    Rgb24,
    /// Little endian RGB
    Bgr32,
    /// Little endian RGB with 24bits per pixel.
    Bgr24,
    /// Video hardware is using a custom pixel format.
    /// Masks for each channel are given below.
    /// `reserved` contains bits which are ignored.
    /// The `xor` and `or` of all fields bits should be [u32::MAX]
    ColourMask {
        red: u32,
        green: u32,
        blue: u32,
        reserved: u32,
    },
}

#[cfg(feature = "uefi")]
pub type UefiMemoryMap = uefi::table::boot::MemoryMap<'static>;

#[non_exhaustive]
pub enum MemoryMap {
    #[cfg(feature = "uefi")]
    Uefi(UefiMemoryMap),

    /// ## Safety
    ///
    /// The kernel must ensure that the MultiBoot information struct is not modified while this is referenced
    #[cfg(feature = "multiboot2")]
    Multiboot2(Multiboot2PmMemoryState),
}

#[non_exhaustive]
pub enum MapIter<'a> {
    #[cfg(feature = "uefi")]
    Uefi(uefi::table::boot::MemoryMapIter<'a>),
    #[cfg(feature = "multiboot2")]
    Multiboot2(Multiboot2PmMemoryStateIter<'a>),
}

impl MemoryMap {
    /// Returns an iterator over the memory map
    ///
    /// # Safety
    ///
    /// Some variants have safety requirements which must be upheld.
    pub unsafe fn iter(&self) -> MapIter {
        match self {
            #[cfg(feature = "uefi")]
            Self::Uefi(u) => MapIter::Uefi(u.entries()),
            #[cfg(feature = "multiboot2")]
            Self::Multiboot2(m) => MapIter::Multiboot2(m.iter()),
        }
    }

    /// Modifies the memory map to ensure that each region is page aligned.
    pub fn sanitize(&mut self) {
        match self {
            #[cfg(feature = "uefi")]
            MemoryMap::Uefi(_) => {} // already sanitized
            Self::Multiboot2(_) => {} // sanitizes inline
        }
    }
}

#[derive(Debug)]
pub struct MemoryRegion {
    pub phys_addr: u64,
    pub size: u64,
    pub ty: MemoryRegionType,
    pub distinct: MemoryRegionDistinct,
}

#[cfg(feature = "uefi")]
impl From<uefi::table::boot::MemoryDescriptor> for MemoryRegion {
    fn from(value: uefi::table::boot::MemoryDescriptor) -> Self {
        let ty = match value.ty {
            MemoryType::LOADER_DATA => MemoryRegionType::Bootloader,

            MemoryType::RESERVED
            | MemoryType::UNUSABLE
            | MemoryType::MMIO
            | MemoryType::RUNTIME_SERVICES_CODE
            | MemoryType::RUNTIME_SERVICES_DATA
            | MemoryType::ACPI_RECLAIM
            | MemoryType::ACPI_NON_VOLATILE
            | MemoryType::PAL_CODE
            | MemoryType::MMIO_PORT_SPACE => MemoryRegionType::Unusable(value.ty.0),

            MemoryType::CONVENTIONAL
            | MemoryType::BOOT_SERVICES_DATA
            | MemoryType::BOOT_SERVICES_CODE
            | MemoryType::LOADER_CODE => MemoryRegionType::Usable,

            e => MemoryRegionType::Unknown(e.0),
        };

        Self {
            phys_addr: value.phys_start,
            size: value.page_count * PAGE_SIZE as u64,
            ty,
            distinct: MemoryRegionDistinct::Uefi {
                ty: value.ty,
                virt_addr: value.virt_start,
                attribute: value.att,
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
    },

    E820(E820Type),
}

#[repr(u32)]
#[derive(Clone, Copy, Debug)]
pub enum E820Type {
    Usable = 1,
    Reserved = 2,
    AcpiReclaimable = 3,
    AcpiNonVolatileStorage = 4,
    BadRam = 5,
    Other(u32),
}

impl From<MemoryAreaType> for E820Type {
    fn from(ty: MemoryAreaType) -> Self {
        match ty {
            MemoryAreaType::Available => Self::Usable,
            MemoryAreaType::Reserved => Self::Reserved,
            MemoryAreaType::AcpiAvailable => Self::AcpiReclaimable,
            MemoryAreaType::ReservedHibernate => Self::AcpiNonVolatileStorage,
            MemoryAreaType::Defective => Self::BadRam,
            MemoryAreaType::Custom(n) => Self::Other(n),
        }
    }
}

impl<'a> Iterator for MapIter<'a> {
    type Item = MemoryRegion;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            #[cfg(feature = "uefi")]
            MapIter::Uefi(i) => Some(i.next()?.clone().into()),
            MapIter::Multiboot2(i) => i.next(),
        }
    }
}

#[cfg(feature = "multiboot2")]
pub struct Multiboot2PmMemoryState {
    pub(crate) mbi_region: core::ops::Range<u64>,
    pub(crate) elf_sections: &'static multiboot2::ElfSectionsTag,
    pub(crate) mem_map: &'static multiboot2::MemoryMapTag,
    pub(crate) used_boundary: u64,
    pub(crate) low_boundary: u64,
}

#[cfg(feature = "multiboot2")]
impl Multiboot2PmMemoryState {
    fn iter(&self) -> Multiboot2PmMemoryStateIter {
        Multiboot2PmMemoryStateIter {
            parent: self,
            next_addr: 0,
            last_index: 0,
            hit_boundary: false,
            hit_low_boundary: false,
        }
    }
}

#[cfg(feature = "multiboot2")]
pub struct Multiboot2PmMemoryStateIter<'a> {
    parent: &'a Multiboot2PmMemoryState,
    next_addr: u64,
    last_index: usize,
    hit_boundary: bool,
    hit_low_boundary: bool,
    // All memory below the low boundary was freed prior to handing control to calling [crate::hatcher_entry]
}

#[cfg(feature = "multiboot2")]
impl Multiboot2PmMemoryStateIter<'_> {
    /// Determines if `range` is occupied and if so how much.
    ///
    /// The range bounds should be page aligned.
    fn address_occupied(
        &self,
        mut range: core::ops::Range<u64>,
    ) -> (core::ops::Range<u64>, MemoryRegionType) {
        for i in self.parent.elf_sections.sections() {
            let start = x86_64::align_down(i.start_address(), PAGE_SIZE as u64);
            let end = x86_64::align_up(i.end_address(), PAGE_SIZE as u64);

            let (b, ty) = Self::cmp_range(range, start..end);
            range = b;
            if ty == MemoryRegionType::Bootloader {
                return (range, ty);
            }
        }

        Self::cmp_range(range, self.parent.mbi_region.clone())
    }

    fn cmp_range(
        memory_range: core::ops::Range<u64>,
        cmp_range: core::ops::Range<u64>,
    ) -> (core::ops::Range<u64>, MemoryRegionType) {
        use core::ops::RangeBounds as _;
        use ranges::{Arrangement, GenericRange};
        let memory_range = ranges::GenericRange::from(memory_range);
        let cmp_range = ranges::GenericRange::from(cmp_range);

        let (range, ty) = match memory_range.arrangement(&cmp_range) {
            Arrangement::Disjoint { .. } | Arrangement::Touching { .. } => {
                (memory_range, MemoryRegionType::Usable)
            }
            Arrangement::Overlapping {
                self_less: true, ..
            } => {
                let start = memory_range.start_bound().map(|e| *e);
                let core::ops::Bound::Included(end) = cmp_range.start_bound().map(|e| *e) else {
                    panic!()
                };
                let end = core::ops::Bound::Excluded(end);

                (
                    GenericRange::new_with_bounds(start, end),
                    MemoryRegionType::Usable,
                )
            }
            Arrangement::Overlapping {
                self_less: false, ..
            } => {
                let start = memory_range.start_bound().map(|e| *e);
                let end = cmp_range.end_bound().map(|e| *e);

                (
                    GenericRange::new_with_bounds(start, end),
                    MemoryRegionType::Bootloader,
                )
            }

            Arrangement::Containing { self_shorter: true } => {
                (memory_range, MemoryRegionType::Bootloader)
            }
            Arrangement::Containing {
                self_shorter: false,
            } => {
                let start = memory_range.start_bound().map(|e| *e);
                let core::ops::Bound::Included(end) = cmp_range.start_bound().map(|e| *e) else {
                    panic!()
                };
                let end = core::ops::Bound::Excluded(end);

                (
                    GenericRange::new_with_bounds(start, end),
                    MemoryRegionType::Usable,
                )
            }
            Arrangement::Starting { self_shorter, .. } => {
                let start = memory_range.start_bound().map(|e| *e);
                let end = if self_shorter {
                    memory_range.end_bound().map(|e| *e)
                } else {
                    memory_range.end_bound().map(|e| *e)
                };

                (
                    GenericRange::new_with_bounds(start, end),
                    MemoryRegionType::Bootloader,
                )
            }

            Arrangement::Ending {
                self_shorter: false,
                ..
            } => {
                let start = memory_range.start_bound().map(|e| *e);
                let core::ops::Bound::Included(end) = cmp_range.start_bound().map(|e| *e) else {
                    panic!()
                }; // sub one because this is inclusive
                let end = core::ops::Bound::Excluded(end);

                (
                    GenericRange::new_with_bounds(start, end),
                    MemoryRegionType::Usable,
                )
            }

            Arrangement::Ending {
                self_shorter: true, ..
            } => (memory_range, MemoryRegionType::Bootloader),

            Arrangement::Equal => (cmp_range, MemoryRegionType::Bootloader),
            Arrangement::Empty { .. } => {
                panic!()
            } // invalid inputs
        };

        let core::ops::Bound::Included(start) = range.start_bound().map(|e| *e) else {
            unreachable!()
        };
        let core::ops::Bound::Excluded(end) = range.end_bound().map(|e| *e) else {
            unreachable!()
        };

        (start..end, ty)
    }
}

#[cfg(feature = "multiboot2")]
impl<'a> Iterator for Multiboot2PmMemoryStateIter<'a> {
    type Item = MemoryRegion;

    fn next(&mut self) -> Option<Self::Item> {
        // If the last available page is this region is next_address then this region is depleted
        if x86_64::align_down(
            self.parent.mem_map.memory_areas()[self.last_index].end_address(),
            PAGE_SIZE as u64,
        ) == self.next_addr
        {
            self.last_index += 1;
        };
        let area = self.parent.mem_map.memory_areas().get(self.last_index)?;
        let start = x86_64::align_up(area.start_address(), PAGE_SIZE as u64);
        let end = x86_64::align_down(area.end_address(), PAGE_SIZE as u64);

        let (mut range, mut ty) = self.address_occupied(start..end);

        if range.contains(&self.parent.low_boundary) {
            range.end = self.parent.low_boundary;
            self.next_addr = self.parent.low_boundary;
            self.hit_low_boundary = true;
            assert_eq!(
                ty,
                MemoryRegionType::Usable,
                "Bug in {} memory below low_boundary was marked as not-free",
                core::any::type_name::<Self>()
            );
            ty = MemoryRegionType::Usable;
        } else if range.contains(&self.parent.used_boundary) {
            range.end = self.parent.used_boundary;
            self.next_addr = self.parent.used_boundary;
            self.hit_low_boundary = true;
            assert_eq!(
                ty,
                MemoryRegionType::Usable,
                "Bug in {} memory below low_boundary was marked as not-free",
                core::any::type_name::<Self>()
            );
            ty = MemoryRegionType::Bootloader;
        } else {
            ty = match (self.hit_low_boundary, self.hit_boundary) {
                (false, false) => MemoryRegionType::Usable, // no boundary hit yet
                (true, false) => MemoryRegionType::Bootloader, // Occupied by libbatcher allocated data
                (true, true) => MemoryRegionType::Usable,
                (false, true) => core::unreachable!(),
            };
        }

        self.next_addr = range.end;

        let distinct_region: MemoryAreaType = area.typ().into();
        Some(MemoryRegion {
            phys_addr: range.start,
            size: range.end - range.start,
            ty,
            distinct: MemoryRegionDistinct::E820(distinct_region.into()),
        })
    }
}
