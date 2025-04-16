use core::fmt::Write;
use core::mem::MaybeUninit;
use uefi::Status;
use uefi::table::boot::MemoryType;
use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Translate};

type SysTable = uefi::table::SystemTable<uefi::table::Boot>;

use crate::variables::*;

const _ASSERT: () = {
    assert!(STACK_SIZE & 0xfff == 0, "STACK_SIZE must be page aligned");
    assert!(core::mem::align_of::<crate::boot_info::BootInfo>() <= 8);
};

// We cannot const BitOR the flags, so we use a macro and do it anyway.
macro_rules! offset_mem_flags {
    () => {
        ::x86_64::structures::paging::PageTableFlags::PRESENT
            | ::x86_64::structures::paging::PageTableFlags::WRITABLE
    };
}

// This converts the Multiboot2 call to a C call
// The jump on the last line throws a linker error if `_kernel_preload_entry_mb2efi64` can't be truncated to a 32bit pointer
// I cannot figure out any other way to assert this
#[cfg(all(feature = "uefi"))]
core::arch::global_asm!(
    r#"
.section .text.hatcher.entry.mb2_efi64
.global _kernel_preload_entry_mb2efi64
_kernel_preload_entry_mb2efi64:
    xor rdi,rdi
    mov edi,eax
    xor rsi,rsi
    mov esi,ebx
    jmp _kernel_mb2_preload_efi64
    .code32
    jmp _kernel_preload_entry_mb2efi64
"#
);

unsafe extern "C" {
    #[deny(clippy::disallowed_methods)]
    pub fn _kernel_preload_entry_mb2efi64() -> !;
}

/// Multiboot2 64-bit entry for the kernel.
/// In the future there might be multiple of these for other legacy BIOS support.
/// The actual entry is [_kernel_preload_entry_mb2efi64] which sets up a C-abi call to this fn
// not here that mbi_ptr is a 32-bit pointer but _kernel_preload_entry ensures that the top half of rsi is clear
#[cfg(all(feature = "uefi"))]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.hatcher.multiboot2.kernel_preload_efi64")]
extern "C" fn _kernel_mb2_preload_efi64(
    magic: u32,
    mbi_ptr: *const multiboot2::BootInformationHeader,
) -> ! {
    assert_eq!(
        magic, 0x36d76289,
        "Wrong magic expected 0x36d76289 for multiboot2 got {:#x}",
        magic
    );
    let mbi = unsafe { multiboot2::BootInformation::load(mbi_ptr) }.unwrap();
    assert!(
        mbi.efi_bs_not_exited_tag().is_some(),
        "Boot services exited"
    );

    // If this panics there is really nothing I can do about it. But it shouldn't ever panic.
    let mut st = unsafe {
        SysTable::from_ptr(mbi.efi_sdt64_tag().unwrap().sdt_address() as *mut _).unwrap()
    };

    let handle = {
        if let Some(h) = mbi.efi_ih64_tag().take() {
            throw(
                &mut st,
                unsafe { uefi::Handle::from_ptr(h.image_handle() as *mut core::ffi::c_void) }
                    .ok_or("Image handle pointer is null"),
            )
        } else {
            efi_panic(&mut st, "Not EFI image handle provided by bootloader")
        }
    };

    unsafe { st.boot_services().set_image_handle(handle) }

    let new_stack = alloc_new_stack(&mut st);

    // SAFETY: Does not dereference slice
    #[cfg(feature = "debug-bits")]
    let _ = unsafe {
        core::writeln!(
            st.stderr(),
            "New stack: {:p}, size: {:#x} top: {:p}",
            new_stack.get_ptr(),
            new_stack.as_slice().len(),
            new_stack.as_slice()
        )
    };
    // allocate this before own_l4 because that will map bi_ptr
    // This is not initialized, do not read
    let bi_ptr: *mut crate::boot_info::BootInfo = {
        let r = st.boot_services().allocate_pool(
            MemoryType::LOADER_DATA,
            core::mem::size_of::<crate::boot_info::BootInfo>(),
        );
        throw(&mut st, r).cast()
    };

    let mut mapper = own_l4(&mut st);
    // SAFETY: l4 is allocated above and UEFI always identity maps memory
    let graphic_info = get_gop_data(&mut st);

    if let Some(ref g) = graphic_info {
        map_framebuffer(&mut st, &mut mapper, g)
    }

    #[cfg(feature = "debug-bits")]
    for i in mbi.elf_sections().unwrap() {
        let _ = core::writeln!(
            st.stderr(),
            "Section: {}, {:?}, start: {:#x}, len: {:#x}, flags: {:#x}",
            i.name().unwrap(),
            i.section_type(),
            i.start_address(),
            i.size(),
            i.flags()
        );
    }

    if let Some(false) = check_kernel_is_mapped(&mbi, &mapper) {
        efi_panic(&mut st, "Kernel code not mapped");
    }

    let mm_entry_size = st.boot_services().memory_map_size().entry_size;
    let (st, mut map) = st.exit_boot_services(MemoryType::LOADER_DATA);

    map.sort();
    let map = {
        // We relocate the pointer to point into the offset physical memory
        // This is because we cannot map the new memory map into the existing `mapper` due to HalfArsedFrameAllocator not working anymore
        // Trying to map this properly will be a massive pain in the arse, so we just don't,
        // however the map is required to initialize memory, so it needs to be given and this is the only way re can really do it
        let count = map.entries().len();
        let ptr = map.get_mut(0).unwrap() as *mut _ as *mut u8;
        // SAFETY: The pointer is not valid until after `mapper` is loaded. We can guarantee that ptr will be valid then
        let slice = unsafe {
            core::slice::from_raw_parts_mut(
                ptr.offset(PHYS_OFFSET_ADDR as isize),
                mm_entry_size * count,
            )
        };
        uefi::table::boot::MemoryMap::from_raw(slice, mm_entry_size)
    };

    let bi = crate::boot_info::BootInfo {
        physical_address_offset: PHYS_OFFSET_ADDR as u64,
        memory_map: Some(crate::boot_info::MemoryMap::Uefi(map)),
        optionals: crate::boot_info::BootInfoOptionals {
            mb2_info: Some(mbi),
            efi_system_table: Some(st),
            graphic_info,
            ..Default::default()
        },
    };
    // SAFETY: bi_ptr is valid for writes and correctly aligned.
    // we hand over a pointer because the stack will go out of scope.
    unsafe { bi_ptr.write(bi) };

    // We will not call set_virtual_address_map() here because it can only be called once, and the kernel may not want to use our map.
    // can I make this unconditional jump instead of call
    cx_switch(new_stack, mapper, bi_ptr)
    // I would put something here in case _hatcher_entry DOES return but lldb does that for me
    // Here will be either ud2 or int3 instructions
    // So the worst that can happen is exceptions, and not some random code being executed
}

fn alloc_new_stack(st: &mut SysTable) -> crate::common::StackPointer {
    // + 2 allows for a guard page at each end.
    let pages = (STACK_SIZE / 4096) + 2;
    let ptr = throw(
        st,
        st.boot_services().allocate_pages(
            uefi::table::boot::AllocateType::AnyPages,
            MemoryType::LOADER_DATA,
            pages,
        ),
    );

    // SAFETY: This is safe the pointer is re-located to the actual base later
    // This acts as setting up guard pages
    unsafe { throw(st, st.boot_services().free_pages(ptr, 1)) };
    unsafe {
        throw(
            st,
            st.boot_services()
                .free_pages(ptr + 4096 + STACK_SIZE as u64, 1),
        )
    };

    // + 4k for guard page, this will be unmapped freed later
    // SAFETY: This is safe, the pointer points to valid memory.
    unsafe {
        crate::common::StackPointer::new_from_bottom((ptr + 4096) as usize as *mut (), STACK_SIZE)
    }
}

/// Checks that the kernel is mapped into `mapper`
///
/// - Returns `None` if this can't be checked. Which should be treated as success
/// - Returns `Some(false)` on failure
/// - Returns `Some(true)` on success
fn check_kernel_is_mapped(
    mbi: &multiboot2::BootInformation,
    mapper: &x86_64::structures::paging::OffsetPageTable,
) -> Option<bool> {
    use x86_64::structures::paging::mapper::TranslateResult;
    for i in mbi.elf_sections()? {
        if let TranslateResult::Mapped { .. } =
            mapper.translate(x86_64::VirtAddr::new(i.start_address()))
        {
            if let TranslateResult::Mapped { .. } =
                mapper.translate(x86_64::VirtAddr::new(i.end_address()))
            {
                break;
            }
        }
        return Some(false);
    }
    Some(true)
}

/// Creates a new [Mapper]
/// Returns a reference to the new table.
/// This will write the physical address of the old table in entry 255 of the new table with all
/// flags clear so the memory may be freed later if possible.
#[cfg(all(feature = "uefi"))]
fn own_l4<'s>(
    st: &'s mut SysTable,
) -> x86_64::structures::paging::mapper::OffsetPageTable<'static> {
    use x86_64::structures::paging::{
        Page, PhysFrame, Size2MiB, Size4KiB, page::PageRangeInclusive, page_table::PageTable,
    };

    // current l4 may be read only
    let new_l4: &mut PageTable = unsafe {
        &mut *(st
            .boot_services()
            .allocate_pages(
                uefi::table::boot::AllocateType::AnyPages,
                MemoryType::LOADER_DATA,
                1,
            )
            .map_err(|_| efi_panic(st, "System ran out of memory"))
            .unwrap() as usize as *mut _)
    };
    *new_l4 = PageTable::new();

    // Re-map the identity mapped addresses to their new offset in higher half
    let mut mapper = unsafe {
        x86_64::structures::paging::mapper::OffsetPageTable::new(
            &mut *new_l4,
            x86_64::VirtAddr::new(0),
        )
    };
    let mut mm = get_mem_map(st);
    mm.sort();

    #[cfg(feature = "debug-bits")]
    for i in mm.entries() {
        let _ = core::writeln!(st.stdout(), "{i:x?}");
    }

    // Compiler doesn't know that this will either be initialized or panic
    let mut last_region = MaybeUninit::uninit();
    // We need to get the last accessible byte of memory
    // There may be entries off the end of accessible memory
    // we need identify these and check the region below it.
    // We identify it by it having no memory attributes. I'm not sure if this is the best idea though.
    for i in (0..mm.entries().len()).rev() {
        if let Some(r) = mm.get(i) {
            if !r.att.is_empty() {
                last_region.write(*r);
                break;
            }
        }
        if i == 0 {
            efi_panic(st, "Cannot determine amount of available memory")
        }
    }

    // SAFETY: Loop above panics if this is not initialized
    let last_region = unsafe { last_region.assume_init() };

    let last_byte = last_region.phys_start + (last_region.page_count * 4096) - 1;

    let range = PageRangeInclusive {
        start: Page::<Size2MiB>::containing_address(x86_64::VirtAddr::new(0)),
        end: Page::<Size2MiB>::containing_address(x86_64::VirtAddr::new(last_byte)),
    };

    let offset = x86_64::VirtAddr::new(PHYS_OFFSET_ADDR as u64);

    for i in range {
        i.start_address().as_u64();
        // SAFETY: This is safe. This is not live.
        unsafe {
            mapper
                .map_to(
                    Page::<Size2MiB>::containing_address(offset + i.start_address().as_u64()),
                    PhysFrame::containing_address(x86_64::PhysAddr::new(
                        i.start_address().as_u64(),
                    )),
                    offset_mem_flags!(),
                    &mut HalfArsedFrameAllocator { st },
                )
                .unwrap()
                .ignore();
        }
    }

    // map lower addresses so everything isn't broken
    for (e, i) in mm.entries().enumerate() {
        match i.ty {
            MemoryType::LOADER_DATA => {
                #[cfg(feature = "debug-bits")]
                let _ = core::writeln!(st.stderr(), "Mapping {e}: {i:x?}");

                // This contains both the kernel and BootInfo data, all of these must be mapped.
                // The kernel can figure out later what can be reclaimed.
                // use 4K pages using 2M may cause issues. todo Maybe optimize this later?
                let iter = unsafe {
                    x86_64::structures::paging::frame::PhysFrameRangeInclusive {
                        start: PhysFrame::<Size4KiB>::from_start_address_unchecked(
                            x86_64::PhysAddr::new(i.phys_start),
                        ),
                        end: PhysFrame::from_start_address_unchecked(x86_64::PhysAddr::new(
                            i.phys_start + (i.page_count * 4096),
                        )),
                    }
                };

                for frame in iter {
                    let b = unsafe {
                        mapper.identity_map(
                            frame,
                            offset_mem_flags!(),
                            &mut HalfArsedFrameAllocator { st },
                        )
                    };
                    throw(st, b).ignore();
                }
            }
            _ => {} // Do nothing. Maybe do something at some point?
        }
    }
    mapper
}

#[cfg(all(feature = "uefi"))]
struct HalfArsedFrameAllocator<'boot> {
    st: &'boot mut SysTable,
}

#[cfg(all(feature = "uefi"))]
unsafe impl<'a> FrameAllocator<x86_64::structures::paging::Size4KiB>
    for HalfArsedFrameAllocator<'a>
{
    fn allocate_frame(
        &mut self,
    ) -> Option<x86_64::structures::paging::PhysFrame<x86_64::structures::paging::Size4KiB>> {
        use uefi::table::boot;
        use x86_64::structures::paging::PhysFrame;

        // SAFETY: The unsafe op is effectively just a typecast
        self.st
            .boot_services()
            .allocate_pages(boot::AllocateType::AnyPages, MemoryType::LOADER_DATA, 1)
            .ok()
            .map(|a| unsafe { PhysFrame::from_start_address_unchecked(x86_64::PhysAddr::new(a)) })
    }
}

#[cfg(all(feature = "uefi"))]
fn get_mem_map<'a, 'b>(st: &'a mut SysTable) -> uefi::table::boot::MemoryMap<'b> {
    let mut extra = 0;
    loop {
        let size = st.boot_services().memory_map_size();
        let real_size = size.entry_size * (size.map_size + extra);

        // map_err may panic but unwrap() never will
        // SAFETY: This is safe because the pointer is guaranteed by the firmware to be valid and real_size
        let b = unsafe {
            core::slice::from_raw_parts_mut(
                throw(
                    st,
                    st.boot_services()
                        .allocate_pool(MemoryType::LOADER_DATA, real_size),
                ),
                real_size,
            )
        };
        let bbc = &mut b[0] as *mut u8; // bamboozle borrow checker
        let mm = st.boot_services().memory_map(b);
        match mm {
            // im not sure if len of hte map is the same as the len of the buffer, so I will return the buffer too, so I can free it later.
            Ok(r) => break r,
            Err(e) if e.status() == Status::BAD_BUFFER_SIZE => {
                extra += 1;
                // SAFETY: This is safe, it frees the buffer just allocated
                // If this returns err then the firmware is faulty
                let _ = unsafe { st.boot_services().free_pool(bbc) };
            }
            Err(_) => efi_panic(st, "Error requesting memory map"),
        }
    }
}

/// Returns graphical info given by the GOP driver.
// todo allow configuring framebuffer
// how to pass config info tho?
// a proc macro would work but would also be annoying.
#[cfg(all(feature = "uefi"))]
fn get_gop_data(st: &mut SysTable) -> Option<crate::boot_info::GraphicInfo> {
    use uefi::proto::console::gop::PixelFormat;

    // SAFETY: This is safe because this is dropped at the end of the function and `exit_boot_services()` is not called
    let g_st = unsafe { st.unsafe_clone() };
    // uefi should provide a convenience function for this
    let mut gop: uefi::table::boot::ScopedProtocol<uefi::proto::console::gop::GraphicsOutput> = {
        let handle = st
            .boot_services()
            .get_handle_for_protocol::<uefi::proto::console::gop::GraphicsOutput>()
            .ok()?;

        let g = g_st.boot_services().open_protocol_exclusive(handle);

        throw(st, g)
    };

    let mode = gop.current_mode_info();

    let pf = match mode.pixel_format() {
        PixelFormat::Rgb => crate::boot_info::PixelFormat::Rgb32,
        PixelFormat::Bgr => crate::boot_info::PixelFormat::Bgr32,
        PixelFormat::Bitmask => {
            if let Some(uefi::proto::console::gop::PixelBitmask {
                red,
                green,
                blue,
                reserved,
            }) = mode.pixel_bitmask()
            {
                crate::boot_info::PixelFormat::ColourMask {
                    red,
                    green,
                    blue,
                    reserved,
                }
            } else {
                efi_panic(
                    st,
                    "Pixel mode specified as custom-bitmask, but did not return a pixel format when queried.",
                );
            }
        }
        PixelFormat::BltOnly => {
            let _ = writeln!(st.stderr(), "[Warn] mode is {:?}", PixelFormat::BltOnly);
            return None;
        }
    };

    if gop.frame_buffer().size() == 0 {
        efi_panic(st, "Framebuffer size is 0\nSomethin' ain't right here");
    }

    // SAFETY: As long as the firmware doesn't lie then this is safe
    Some(crate::boot_info::GraphicInfo {
        height: mode.resolution().1 as u64,
        width: mode.resolution().0 as u64,
        stride: mode.stride() as u64,
        pixel_format: pf,
        framebuffer: unsafe {
            core::slice::from_raw_parts_mut(
                gop.frame_buffer().as_mut_ptr(),
                gop.frame_buffer().size(),
            )
        },
    })
}

fn map_framebuffer(
    st: &mut SysTable,
    mapper: &mut x86_64::structures::paging::mapper::OffsetPageTable,
    fb_info: &super::boot_info::GraphicInfo,
) {
    let b = &fb_info.framebuffer;
    if b.len() > 0x200000 {
        // This may be problematic if it overruns the device memory, but with a PCI VGA device it is unlikely to.
        let range = x86_64::structures::paging::page::PageRangeInclusive::<
            x86_64::structures::paging::Size2MiB,
        > {
            start: Page::containing_address(x86_64::VirtAddr::from_ptr(&b[0])),
            end: Page::containing_address(x86_64::VirtAddr::from_ptr(b.last().unwrap())),
        };
        for i in range {
            // never none?
            unsafe {
                mapper.identity_map(
                    x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size2MiB>::from_start_address_unchecked(x86_64::PhysAddr::new(i.start_address().as_u64())),
                    PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE | PageTableFlags::PRESENT,
                    &mut HalfArsedFrameAllocator { st })
            }.map_err(|_| efi_panic(st, "Failed to map framebuffer")).unwrap().ignore();
        }
    }
}

fn cx_switch(
    stack_pointer: crate::common::StackPointer,
    mapper: x86_64::structures::paging::mapper::OffsetPageTable,
    bi: *mut crate::boot_info::BootInfo,
) -> ! {
    let l4 = mapper.level_4_table();
    let sp = stack_pointer.get_ptr();
    let entry = super::_hatcher_entry;

    unsafe {
        core::arch::asm!(
            "mov cr3,{l4}",
            "mov rsp,{sp}",
            "jmp {entry}",
            l4 = in(reg) l4,
            sp = in(reg) sp,
            in("rdi") bi,
            entry = in(reg) entry,
            options(noreturn),
        )
    }
}

/// Panics.
///
/// This should be used instead of the normal panic!() macro, this will print an error to the UEFI
/// stderr instead of calling the kernel's (possibly uninitialized) panic handler.
///
/// This will absolutely leak memory
///
/// # Safety
///
/// This fn can safely be called with a cloned [SysTable], because this does not return and does
/// not call [SysTable::exit_boot_services]
#[track_caller]
#[cfg(all(feature = "uefi"))]
fn efi_panic<E: core::fmt::Display>(st: &mut SysTable, err: E) -> ! {
    let _ = core::writeln!(st.stderr(), "Panic at {}", core::panic::Location::caller());
    let _ = core::writeln!(st.stderr(), "{err}");

    // I hope using a nullptr doesn't break the UEFI
    unsafe {
        st.boot_services().exit(
            st.boot_services().image_handle(),
            Status::ABORTED,
            0,
            core::ptr::null_mut(),
        )
    };
}

#[track_caller]
#[cfg(feature = "uefi")]
fn throw<T, E: core::fmt::Debug>(st: &mut SysTable, o: Result<T, E>) -> T {
    match o {
        Err(e) => {
            let _ = core::writeln!(st.stderr(), "Panic at {}", core::panic::Location::caller());
            let _ = core::writeln!(st.stderr(), "Attempted to unwrap `Result::Err({e:?})`");
            unsafe {
                st.boot_services().exit(
                    st.boot_services().image_handle(),
                    Status::ABORTED,
                    0,
                    core::ptr::null_mut(),
                )
            };
        }
        Ok(e) => e,
    }
}

pub(crate) mod pm {
    unsafe extern "C" {
        pub fn hatcher_multiboot2_pm_entry() -> !;
    }
    extern "C" fn hatcher_entry_mb2pm(mbi: *mut multiboot2::BootInformationHeader) -> ! {
        // I cant remember if switching to long mode clears the higher half of the register, so clear the higher bits anyway;
        let mbi =
            (mbi.addr() & (suffix::metric!(4Gi) - 1)) as *mut multiboot2::BootInformationHeader;

        panic!()
    }

    use core::arch::global_asm;

    global_asm!(
        "
        .section .text.hatcher.entry.multiboot2.protected_mode
        .code32
        .global hatcher_multiboot2_pm_entry

        //.equ entry_64_offset, hatcher_multiboot2_pm_entry-{entry_64}

        .equ entry_pointer_offset, hatcher_multiboot2_pm_entry-.L_get_eip

        .equ initial_gdt_offset,initial_gdt-hatcher_multiboot2_pm_entry

        .equ mem_map_base_offset, mem_map_base - hatcher_multiboot2_pm_entry

        .equ elf_headers_base_offset, elf_headers_base - hatcher_multiboot2_pm_entry

        .equ alloc_counter_offset, alloc_counter - hatcher_multiboot2_pm_entry

        initial_gdt:
            .8byte 0
            .8byte {KNL_CODE_SEG_BITS}
            .8byte {KNL_DATA_SEG_BITS}

        alloc_counter:
            .4byte 0

        mem_map_base:
            .4byte 0

        elf_headers_base:
            .4byte 0

        initial_stack:
            .fill 32,4,0

        /// This handles switching the CPU to long mode, and handing over to rust code.
        /// ebx will point to the MBI structure until rust code is called.
        hatcher_multiboot2_pm_entry:
            // Assert the magic number is correct

            cmp eax,{MULTIBOOT2_MAGIC}
            jnz .L_fail

            // we need to use att because using intel causes this to be sub ebp, dword ptr [symbol] regardless of what it try to do about it.
            call .L_get_eip

            .L_get_eip:
            pop ebp

            lea ebp,[entry_pointer_offset] // locates the address of hatcher_multiboot2_pm_entry if we are relocated

            mov ecx,{CR0_INITIAL}
            mov cr0,ecx
            mov ecx,{CR4_INITIAL}
            mov cr4,ecx
            mov esp,ebp // points to the top of the stack

            mov ecx,{MEMORY_MAP_TAG}

            call __hatcher_mb2pm_tag_finder


        // We need to allocate memory.
        // To do this we need to parse memory map, and compare it with the ELF headers and MBI address
        // Here we locate the tags for the ELF headers and memory map
            lea edi,[ebp-mem_map_base_offset]
            mov [edi],eax

            // locate elf headers
            mov ecx,{ELF_HEADERS_TAG}
            call __hatcher_mb2pm_tag_finder
            lea edi,[ebp-elf_headers_base_offset]
            mov [edi],eax
        .L_setup_l3_table:
            call __hatcher_mb2pm_alloc_mem
            mov edi,eax
            mov ecx,{GIANT_PAGE_BITS}
            xor esi,esi
            xor edx,edx

        .L_setup_l3_inner:
            mov [edi+esi],ecx
            mov dword ptr [edi+esi+4],edx   // set top half
            add ecx,{GIANT_PAGE_LEN}
            adc edx,0                       // emulate 64bit add
            add esi,8                       // increment entry
            cmp esi,{PAGE}
            jc .L_setup_l3_inner // While page offset < {PAGE} continue


        .L_setup_l4_table:
            push edi
            call __hatcher_mb2pm_alloc_mem
            mov ecx,4096
            mov edi,eax
            push eax
            xor eax,eax
            rep stosb
            pop eax
            pop edi
            or edi,{PAGE_BITS}&0xf // set lower bits
            mov [eax],edi // set first entry
            mov cr3,eax

            /// allocate initial stack
            call __hatcher_mb2pm_alloc_mem
            mov esp,eax
            add esp,{PAGE}


            // setup gdt
            sub esp,6 // 6 bytes for [2+4] [len + address]
            mov word ptr [esp],0x17          // set size for gdt, (2 entries + null entry) - 1 byte
            lea eax,[ebp-initial_gdt_offset]
            mov [esp+2],eax                  // Set pointer into GDT
            lgdt [esp]
            add esp,6

            mov eax,{EFER_BITS}
            xor edx,edx
            mov ecx,{EFER_ADDR}
            wrmsr

            mov eax,cr0
            or eax,0x80000000 // 1 < 31 paging bit bit
            mov cr0,eax
            // we are now in compatibility mode
            long_mode:

            /// Calculate the entry address
            /// Compiler is having a hissy fit about doing this at build time so it's being done at runtime
            lea ecx,[hatcher_multiboot2_pm_entry] // get VMA offset (when not relocated result will be 0)
            sub ecx,ebp // for whatever reason if I try to do this in the above instruction it always adds the symbol and never subs it
            lea eax,{entry_64}
            add ecx,eax // add {entry_64} VMA, this will get the absolute address at runtime
            mov dword ptr [esp-6],ecx
            mov word ptr [esp-2],{KNL_CODE_SEG}

            // set segment registers
            mov cx,{KNL_DATA_SEG}
            mov ds,cx
            mov ss,cx
            mov es,cx
            mov fs,cx
            mov gs,cx

            mov edi,ebx
            jmp fword ptr [esp-6]

        .L_fail:
            ud2

        __hatcher_mb2pm_tag_finder:
            mov esi,8 // Start of first tag

            /// Locates the tag found in `ecx`.
            /// Returns the address of the tag in `eax`.

            .L_tag_finder_inner:
            // Here we iterate over each tag to locate the simple memory tag
            // Locating free memory is important because the first thing we need to do is locate
            // enough memory to allocate a stack and set up page tables to initialize long-mode
            cmp ecx,[ebx+esi]
            jnz .L_wrong_tag

            lea eax,[ebx+esi] // eax contains the address of the tag
            ret

            .L_wrong_tag:
            mov edx,[ebx+esi+4]
            mov eax,8-1 // align mask

            // Align up to 8

            // if len & 8-1 == 0 {{
            //     len & (8-1)
            // }} else {{
            //     (len & (8-1)) + 8
            // }}

            and eax,edx
            and edx,{ALIGN_8_UPPER_MASK} // clears lower 4 bits

            push 8
            cmp eax,0
            cmovne eax,[esp]
            add esp,4

            add edx,eax
            add esi,edx
            jmp .L_tag_finder_inner


        /// Locates and allocates a page from the memory map returned in eax
        __hatcher_mb2pm_alloc_mem:

            // frame setup
            //      0x0: memory entry size
            //      0x4: number of memory-entries
            //      0x8: current memory-entry iteration
            //      0xc: remaining page-found count

            sub esp,4 * 4 // frame size, sizeof<u32>() * words
            mov esi,[ebp - mem_map_base_offset]
            mov edi,[ebp - elf_headers_base_offset]

            lea ecx,[ebp - alloc_counter_offset]
            mov eax,[ecx]
            mov dword ptr [esp+0xc],eax // load alloc counter into esp+0xc

            mov ecx,[esi+8] // entry_size
            mov eax,[esi+4] // tag len
            sub eax,16
            xor edx,edx
            div ecx
            mov dword ptr [esi+4],eax // write number of mem elements to stack

            add esi,16 // move esi to first mem map element

            mov dword ptr [esp+8],0 // iteration is 0

        /// locates the next usable element in the memory map
        .L_alloc_mem_loop:

            mov edx,[esi+4] // address upper half
            test edx,0 // assert that this is not dma64 memory
            jne .L_alloc_mem_loop_continue


            // check that memory-type is usable
            mov edx,[esi+16]
            cmp edx,1
            jne .L_alloc_mem_loop_continue

        .L_alloc_mem_region_loop:
            mov eax,[esi] // lower address half
            mov ecx,[esi+8] // length lower half
            // we dont fuck with the upper half because we cannot access it anyway
            cmp ecx,{PAGE} // check that this is larger than a page
            jb .L_alloc_mem_loop_continue
            .L_alloc_mem_region_loop_restart:
            call .L_align_up
            push esi
            call __hatcher_address_used_check
            pop esi

            sub dword ptr [esp+0xc],1       // decrement remaining-page count
            setnc al // trick algo into failing this address
            bt eax,0

            // jumping here causes address to be aligned upward, and the new address to be
            // re-checked. address_used_check adds one to the address so aligning it up checks the next page
            // todo: optimize the shit out of this, by returning the amount of occupied space in the region in `ecx` on a fail
            jc .L_alloc_mem_region_loop_restart

            add dword ptr [ebp-alloc_counter_offset],1 // next call to __hatcher_mb2pm_alloc_mem must find an extra 'unused' frame

            add esp,4*4
            ret

        /// Aligns eax up to 4096 and checks that the region still contains 4096 bytes
        /// If it doesnt it will return to `.L_alloc_mem_loop_continue`

        .L_align_up:
            mov edx,{PAGE}-1

            and edx,eax
            cmp edx,0
            jz .L_align_up_ret

            and eax,~({PAGE}-1)
            add eax,{PAGE}
            jc .L_align_up_fail

            .L_align_up_ret:
            ret

        /// Failure branch.
        /// Stack frame must be clear
        .L_align_up_fail:
            add esp,4 // drop return address
            jmp .L_alloc_mem_loop_continue

        /// Runs to start the next loop iteration for a mem region
        .L_alloc_mem_loop_continue:
            // increments iteration and check if more entries are expected
            add dword ptr [esp+8],1 // add iteration

            mov eax,[esp] // fetch element size
            add esi,eax // increment element pointer

            mov eax,[esp+8]
            mov ecx,[edi+4]
            cmp eax,ecx // compare `curr_iteration - max_iteration`
            ja .L_fail     // fixme check this is correct (also add fail)
            jmp .L_alloc_mem_loop

        /// Checks that the address in `eax` is not used by the loaded ELF or MBI struct.
        ///
        /// On completion `eax` will contain the requested address and bit 0 will indicate if this
        /// address can be allocated. A value of `0` in bit 0 indicates success and `1` indicates failure
        // setting addr|1 misaligns the pointer which is why it's failure
        //
        // frame layout
        //      0: initial address
        //      4: remaining ELF iterations
        __hatcher_address_used_check:
            sub esp,4 * 2
            mov [esp],eax // save address

            // Load MBI base into `esi`, and last byte into `edi`
            mov esi,ebx
            mov edi,[ebx]
            add edi,ebx // needs to be base+len
            sub edi,1 // needs to compare last byte
            call .L_address_range_check

            bt eax,0 // check if fail bit is set
            jc .L_address_used_elf_ret


            // load elf headers pointer
            lea esi,[ebp-elf_headers_base_offset]
            mov edx,[esi]

            // store number of iterations remaining
            mov ecx,[edx+8]
            mov [esp+4],ecx
            add edx,0x14 // we only load ELF64s so the header is 48bytes long, the specification fails to mention this.

            .L_address_used_elf_sections_iter:
            .L_address_used_elf_sections_iter_continue:
            sub dword ptr [esp+4],1 // decrement to get index. (initial index is len of array)
            jc .L_address_used_elf_ret

            // we iterate in reverse order because its easier
            mov ecx,eax // we need eax for multiplication
            mov eax,64 // just assume size is 64 bytes, that's how long it is for ELF64, and we dont do ELF32 here.
            push edx
            mul dword ptr [esp+8] // returns address of next header (edx saved so we must add 4 to esp offset)
            sub eax,64 // previously eax was nex index, decrement it by one
            pop edx
            xchg eax,ecx // return address to where it belongs

            mov esi,[edx+ecx+0x4]
            cmp esi,0 // determine if section is unused, skip if so
            je .L_address_used_elf_sections_iter_continue

            mov esi,[edx+ecx+0x18] // get ELF section address. We only need lower half because we're guaranteed to be loaded below 4GiB
            mov edi,[edx+ecx+0x28] // ELF section size
            add edi,esi
            sub edi,1 // last byte

            call .L_address_range_check

            bt eax,0 // check if 0 bit is set
            jnc .L_address_used_elf_sections_iter // next iteration

        /// return from address_used_check
        .L_address_used_elf_ret:

            add esp,4 * 2
            ret

        /// Checks that the range described in `esi`:`edi` is not within the page pointed to by `eax`.
        .L_address_range_check:
            mov ecx,eax // copy address
            add ecx,{PAGE}-1 // We need to check the entire frame, so we check against the last byte in the frame
            cmp ecx,esi // sub address by mbi_base
            jc .L_address_range_check_ret // overflows if pointer is below mbi_base.

            cmp eax,edi
            setc al // overflow indicates that address is lower than MBI end, so it must point within the MBI. Set fail bit
            .L_address_range_check_ret:
            ret
        ",
        CR0_INITIAL = const 0x20010001u32,
        CR4_INITIAL = const 0x00000020u32,
        MEMORY_MAP_TAG = const 6u32,
        ELF_HEADERS_TAG = const 9u32,
        MULTIBOOT2_MAGIC = const 0x36d76289u32,
        PAGE = const 0x1000u32,
        GIANT_PAGE_BITS = const 0x83u32, // set present,page_size,writable bits
        GIANT_PAGE_LEN = const suffix::metric!(1Gi),
        PAGE_BITS = const 0x3u32,
        KNL_CODE_SEG = const 8u16,
        KNL_DATA_SEG = const 16u16,
        EFER_BITS = const 0x900u32,
        EFER_ADDR = const 0xC000_0080u32,
        ALIGN_8_UPPER_MASK = const !(8-1),
        KNL_DATA_SEG_BITS = const 0xcf93000000ffffu64,
        KNL_CODE_SEG_BITS = const 0xaf9b000000ffffu64,
        entry_64 = sym hatcher_entry_mb2pm
    );
}
