use core::ops::Add;
use elf::ElfBytes;
use elf::endian::NativeEndian;

pub trait ElfLoad {
    fn map(&self) -> ElfLoadIter<'_>;

    /// Returns the range requested by the ELF file.
    /// The range will indicate the first and last bytes indicated as loaded by the file.
    ///
    /// If the `e_type` field is [elf::abi::ET_EXEC] then this file **must** be loaded into the indicated range.
    /// For other `e_type`'s the range indicates the size of the file.
    ///
    /// All offsets given for this file will be within this range.
    fn load_range(&self) -> core::ops::Range<u64>;

    /// Applies relocations to the loaded image.
    /// The first byte of `loaded` must be the image virtual address `0` and the last byte must be
    /// the last byte of the image.
    unsafe fn relocate(&self, loaded: *mut [u8]) -> Result<(), ()>;
}

impl<'a> ElfLoad for ElfBytes<'a, NativeEndian> {
    fn map(&self) -> ElfLoadIter<'_> {
        ElfLoadIter {
            elf: &self,
            segment: 0,
        }
    }

    fn load_range(&self) -> core::ops::Range<u64> {
        let mut low = u64::MAX;
        let mut high = 0;
        for i in self
            .segments()
            .unwrap()
            .iter()
            .filter(|phdr| phdr.p_memsz != 0)
        {
            low = i.p_vaddr.min(low);
            high = i.p_vaddr.add(i.p_memsz).max(high);
        }
        low..high
    }

    unsafe fn relocate(&self, loaded: *mut [u8]) -> Result<(), ()> {
        for i in self
            .section_headers()
            .unwrap()
            .iter()
            .filter(|shdr| shdr.sh_type == elf::abi::SHT_RELA)
        {
            let t = self.section_data_as_relas(&i).unwrap();
            crate::relocation::relocate_image(loaded, core::ptr::null(), core::ptr::null(), t)
                .unwrap();
        }
        Ok(())
    }
}

pub struct ElfLoadIter<'a> {
    elf: &'a ElfBytes<'a, NativeEndian>,
    segment: usize,
}

impl<'a> Iterator for ElfLoadIter<'a> {
    type Item = LoadSection<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let segments = self.elf.segments()?;
        let _ = segments.get(self.segment).ok()?;
        let location = Some(LoadSection {
            elf: self.elf,
            segment: self.segment,
        });
        self.segment += 1;
        location
    }
}

pub struct LoadSection<'a> {
    elf: &'a ElfBytes<'a, NativeEndian>,
    segment: usize,
}

impl LoadSection<'_> {
    /// Returns an iterator yielding what should be loaded into the indicated load offset.
    pub fn load_seg_iter(&self) -> LoadSectionIter<'_> {
        LoadSectionIter {
            elf: self.elf,
            segment: self.segment,
            index: 0,
            loaded: 0,
        }
    }

    /// Loads the section into memory where `base` points to the base address where the ELF is being loaded.
    /// `base` should be the address of [`ElfLoad::load_range`]`.start` in the loaded image.
    ///
    /// ```
    /// # use std::iter::TrustedRandomAccessNoCoerce;
    /// # use elfload::load::ElfLoad;
    ///
    /// fn load_elf(elf: &elf::ElfBytes<elf::endian::NativeEndian>) -> Box<u8> {
    ///     // Construct box with enough space to store the entire image.
    ///     let image = vec![0u8;elf.load_range().size()].into_boxed_slice()
    ///
    ///     for segment in elf.map() {
    ///         // `base` always points to byte `0` regardless of the actual range of the image
    ///         segment.load(&image[0])
    ///     }
    /// }
    ///
    /// ```
    ///
    /// # Safety
    ///
    /// The caller must ensure that `base` points to the correct base where the ELF file should be loaded.
    /// The caller must also ensure that the region that this section will be loaded into is valid for writes
    pub unsafe fn load(&self, base: *mut u8) -> Result<(), ()> {
        let phdr = self.elf.segments().unwrap().get(self.segment).unwrap();
        let offset_base = self.elf.load_range().start;
        let section_start = offset_base + phdr.p_vaddr;

        // SAFETY: Caller must guarantee that this region can be referenced safely.
        let section = unsafe {
            core::slice::from_raw_parts_mut(
                base.offset((self.offset() - offset_base) as isize),
                phdr.p_memsz as usize,
            )
        };
        section.fill(0);

        for (l, offset) in self.load_seg_iter() {
            match l {
                LoadData::Data(data) => section[(offset - section_start) as usize
                    ..(offset - section_start) as usize + data.len()]
                    .copy_from_slice(data),
                LoadData::Zero => section[offset as usize..].fill(0),
                LoadData::LoadError => return Err(()),
            }
        }
        Ok(())
    }

    /// Returns the section start from the file offset returned by [ElfLoad::offset_base]
    pub fn offset(&self) -> u64 {
        self.elf
            .segments()
            .unwrap()
            .get(self.segment)
            .unwrap()
            .p_vaddr
    }
}

/// An iterator yielding information on how a section should be loaded.
///
/// This iterator returns a [LoadData] indicating the data that should be loaded.
/// This also returns a [u64] which indicates the offset from the ELF load base this data should be loaded.
pub struct LoadSectionIter<'a> {
    elf: &'a ElfBytes<'a, NativeEndian>,
    segment: usize,
    index: usize,

    /// Offset from the base address.
    loaded: usize,
}

impl<'a> Iterator for LoadSectionIter<'a> {
    type Item = (LoadData<'a>, u64);
    fn next(&mut self) -> Option<Self::Item> {
        let seg = self.elf.segments().unwrap().get(self.segment).unwrap();
        // Base address of this section
        let base = self.elf.segments().unwrap().get(self.index).ok()?.p_vaddr;

        if self.loaded == seg.p_memsz as usize {
            return None;
        }

        // We have read all the "file" data into the section
        // Now load all zeros
        if self.loaded as u64 == seg.p_filesz {
            self.loaded = seg.p_memsz as usize;
            return Some((LoadData::Zero, self.loaded as u64 + base));
        }
        let seg_range = seg.p_vaddr..seg.p_offset + seg.p_filesz;

        // Fatass iterator just to only pull one element.
        let (i, shdr) = self
            .elf
            .section_headers()
            .unwrap()
            .iter()
            .enumerate()
            .skip(self.index)
            .filter(|(_, shdr)| seg_range.contains(&shdr.sh_offset))
            .next()?;

        self.index = i + 1;

        match self.elf.section_data(&shdr) {
            Ok((data, None)) => {
                self.loaded += shdr.sh_size as usize;
                return Some((LoadData::Data(data), shdr.sh_addr));
            }
            Ok((_, Some(_))) => {} // Compression not supported yet
            Err(_) => {}
        }

        Some((LoadData::LoadError, u64::MAX))
    }
}

pub enum LoadData<'a> {
    /// Load slice into the section for `len` bytes.
    Data(&'a [u8]),
    /// Completely fill the remainder of the section with zeros.
    /// No more data will be loaded into the section after this.
    Zero,
    /// Loader encountered error. Abort loading.
    LoadError,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::ElfLoad as _;
    use core::ops::Add;
    use elf::ElfBytes;
    use elf::endian::NativeEndian;
    use std::prelude::rust_2024::*;

    #[test]
    fn elf_range() {
        use std::io::Write as _;
        let fname = std::env::args().nth(0).unwrap();

        let mut command = std::process::Command::new("readelf");
        command.arg(&fname).arg("-l");
        let output = command.output().unwrap();
        std::io::stdout().write_all(&output.stdout).unwrap();

        let elf_raw = std::fs::read(&fname).unwrap();
        let elf = ElfBytes::<NativeEndian>::minimal_parse(&elf_raw).unwrap();

        let mut start = u64::MAX;
        let mut end = 0;
        for i in elf.segments().unwrap().iter() {
            start = i.p_vaddr.min(start);
            end = i.p_vaddr.add(i.p_memsz).max(end);
        }

        assert_eq!(start..end, elf.load_range());
    }

    #[test]
    fn count_sections() {
        let fname = std::env::args().nth(0).unwrap();

        let elf_raw = std::fs::read(&fname).unwrap();
        let elf = ElfBytes::<NativeEndian>::minimal_parse(&elf_raw).unwrap();

        assert_eq!(elf.segments().unwrap().len(), elf.map().count())
    }

    #[test]
    fn load_offsets_ok() {
        let fname = std::env::args().nth(0).unwrap();

        let elf_raw = std::fs::read(&fname).unwrap();
        let elf = ElfBytes::<NativeEndian>::minimal_parse(&elf_raw).unwrap();
        let e_range = elf.load_range();

        for section in elf.map() {
            for r in section.load_seg_iter() {
                assert!(e_range.contains(&r.1))
            }
        }
    }
}
