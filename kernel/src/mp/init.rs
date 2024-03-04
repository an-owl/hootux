use alloc::boxed::Box;
use alloc::vec::Vec;
use acpi::platform::ProcessorState;
use x86_msr::MsrFlags;
use x86_msr::Msr;
use crate::interrupts::apic::Apic;

const STACK_SIZE: usize = 0x100000;
const XFER_OFFSET: usize = 0;
const INIT_GDT_OFFSET: usize = 0;
static AP_INIT_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Starts up all available Application Processors.
///
/// # Safety
///
/// This fn is unsafe because the caller must ensure that `tls_data` `tls_file_size` and `tls_data_size`
/// correctly describe the thread local template.
pub(super) unsafe fn start_mp(tls_data: *const u8, tls_file_size: usize, tls_data_size: usize) {
    let acpi = crate::system::sysfs::get_sysfs().firmware().get_acpi().find_table::<acpi::madt::Madt>().unwrap();
    let cpus = acpi.parse_interrupt_model_in(alloc::alloc::Global).unwrap().1.unwrap();
    // tramp_box is freed when the fn exits
    let (addr,mut tramp_box) = allocate_trampoline().expect("Failed to allocate trampoline");
    let init_gdt = addr + (unsafe { &_trampoline_data as *const _ as usize } - _trampoline as *const fn() as usize) as u64;

    let gdt_ptr = GdtPtr{size: 64, ptr: init_gdt.as_u64() as u32 + core::mem::offset_of!(TrampolineData,gdt) as u32}; // size is in bytes

    let mut cache = StartupCache::new(addr);
    // This is safe, this will never be re-written
    // GDT must be in the trampoline region
    let data_region = (init_gdt + &*tramp_box as *const _ as usize).as_mut_ptr::<TrampolineData>();
    // clear data section
    tramp_box[init_gdt.as_u64() as usize..].fill_with(|| 0);

    unsafe { data_region.write_volatile(TrampolineData{
        gdt_ptr,
        gdt: cache.gdt.clone(),
        xfer: Default::default(),
    }) }

    // SAFETY: This is safe because data_region is initialized above
    let tr_data =  unsafe {&mut *data_region };

    log::info!("Starting {} CPUs", cpus.application_processors.len() );
    // count for BSP
    AP_INIT_COUNT.fetch_add(1,atomic::Ordering::Relaxed);

    for i in cpus.application_processors.iter() {

        match i.state {
            ProcessorState::Disabled => { log::warn!("CPU with ACPI-ID {} is disabled",i.local_apic_id) },
            ProcessorState::WaitingForSipi => {
                AP_INIT_COUNT.fetch_add(1,atomic::Ordering::Relaxed);
                bring_up_ap(i.local_apic_id, addr, &cache,tls_data,tls_file_size,tls_data_size,tr_data)
            },
            ProcessorState::Running => panic!("Why are you running? {}",i.local_apic_id),
        }
    }

    unsafe { crate::mem::mem_map::unmap_page(x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::from_start_address(addr).unwrap()) }
    ap_init_sync();
}

/// Brings up the AP specified by `id`.
/// The CPU will be initialized and will stop before running the executor and wait until all CPUs are ready to begin multitasking.
///
/// # Safety
///
/// `tls_data` `tls_file_size` and `tls_data_size` must correctly describe the thread local segment template.
/// These values are given by the bootloader.
/// The frame contained in `trampoline_box` must be the same as the one pointed to by `trampoline_addr`
unsafe fn bring_up_ap(id: super::CpuIndex, trampoline_addr: x86_64::VirtAddr, cache: &StartupCache, tls_data: *const u8, tls_file_size: usize, tls_data_size: usize, trampoline_data: &mut TrampolineData ) {
    use crate::interrupts::{apic,apic::Apic};

    let block = |msec| {
        let init_blk: crate::time::AbsoluteTime = crate::task::util::Duration::millis(10).into();
        while !init_blk.is_future() {
            core::hint::spin_loop()
        }
    };

    log::info!("Bringing up CPU {id}");

    let tls_ptr = crate::mem::thread_local_storage::new_tls(tls_data,tls_file_size,tls_data_size);

    // INIT
    let mut apic = apic::get_apic();
    // SAFETY: This is safe, the target must be reset to prevent UB
    unsafe { apic.send_ipi(apic::IpiTarget::Other(id), apic::InterruptType::Init, 0).unwrap() };
    block(10);

    // SIPI
    let sipi_vector = ((trampoline_addr.as_u64() & 0xff0_0000) >> 12) as u8;
    let mut start_good = false;
    for i in 0..2 {
        unsafe { apic.send_ipi(apic::IpiTarget::Other(id), apic::InterruptType::SIPI, sipi_vector).unwrap() };
        match init_harness(trampoline_data,cache,tls_ptr,) {
            Ok(_) => {
                start_good = true;
                if i > 0  {
                    log::warn!("CPU {id} started on second SIPI attempt")
                }
                break;
            },
            Err(_) => continue,
        }
    }

    if !start_good {
        log::error!("CPU with ACPI-ID {id} failed to start");
        AP_INIT_COUNT.fetch_sub(1,atomic::Ordering::Relaxed);
    }
}

/// A harness for AP startup watches the AP state and provides information that it required for startup.
///
/// This is an "initialization harness" it does not initialize the harness.
///
/// # Safety
///
/// The caller must ensure that `tls_ptr` points to a unique thread local segment that is correctly initialized
unsafe extern "C" fn init_harness(tr_data: &mut TrampolineData, cache: &StartupCache, tls_ptr: *const *const u8) -> Result<(),()> {

    // SAFETY: The box contains the frame the AP uses for startup, ..data_diff() returns the data offset into the frame.
    loop {
        let rq = tr_data.xfer.wait().ok_or(())?;
        match rq {
            DataRequest::Empty => unreachable!(),
            DataRequest::PageAttributeTable => {
                let (h,l) = unsafe { x86_msr::architecture::Pat::read().bits() };
                let bits = (h as u64) << 32 | l as u64;
                tr_data.xfer.send(bits);
                log::trace!("CPU responded");
            }
            DataRequest::TopLevelPageTable => {
                let bits: u32 = cache.l4_addr.as_u64().try_into().expect("Attempted to start AP with DMA64 address");
                tr_data.xfer.send(bits as u64);
            }
            DataRequest::RustEntry => {
                let lmi = x86_64::VirtAddr::from_ptr(long_mode_init as *const ()).as_u64();
                tr_data.xfer.send(lmi);
                break Ok(());
            }
            DataRequest::LongModeStack => {
                log::debug!("AP requested stack");
                let mut v = Vec::new();
                v.resize(STACK_SIZE + 1,0u8); // +1 allows rsp to be aligned (wastes shitloads of memory though). todo make new allocator to fix this
                let ptr = v.leak().last_mut().unwrap() as *mut u8 as usize as u64;
                tr_data.xfer.send(ptr);
                log::debug!("Kernel stack at {ptr:#x}");
            }
            DataRequest::KernelTLS => {
                tr_data.xfer.send(tls_ptr as usize as u64);
            }
        }
    }
}

/// Allocates the trampoline to the 16-bit DMA region, returning a box containing the allocation and
/// its identity mapped linear address
fn allocate_trampoline() -> Result<(x86_64::VirtAddr, Box<[u8;4096],crate::alloc_interface::DmaAlloc>),()> {
    use x86_64::{
        PhysAddr,
        structures::paging::frame::PhysFrame,
        structures::paging::Mapper,
        structures::paging::PageTableFlags,
    };
    // copy _trampoline into `region`
    let tra = _trampoline as *const fn() as *const [u8;4096];
    let region = Box::new_in( unsafe{ *tra } ,crate::alloc_interface::DmaAlloc::new(crate::mem::MemRegion::Mem16,4096));

    // shouldn't panic if it does then the bus is in the allocator or the translator
    let addr = PhysFrame::<x86_64::structures::paging::Size4KiB>::from_start_address(PhysAddr::new(crate::mem::mem_map::translate_ptr(&*region).unwrap())).unwrap();
    let flags = PageTableFlags::PRESENT | PageTableFlags::NO_CACHE | PageTableFlags::WRITABLE ;
    unsafe { crate::mem::SYS_MAPPER.get().identity_map(addr,flags,&mut crate::mem::DummyFrameAlloc).map_err(|e| { log::error!("Unable to load ap trampoline {e:?}"); }) }?.flush();

    Ok((x86_64::VirtAddr::new(addr.start_address().as_u64()),region))
}

struct StartupCache {
    gdt: x86_64::structures::gdt::GlobalDescriptorTable,
    l4_addr: x86_64::PhysAddr,
    l4: Option<Box<x86_64::structures::paging::PageTable,crate::alloc_interface::DmaAlloc>>,
}

impl StartupCache {
    /// Creates a new instance of `Self`.
    ///
    /// # Panics
    ///
    /// This fn will panic if `trampoline_addr` is not page aligned or above 64k.
    fn new(trampoline_addr: x86_64::VirtAddr) -> Self {
        assert_eq!(trampoline_addr.as_u64() & (!0xf000),0, "Bad trampoline address, must be page aligned & below 64K address: {:#x}",trampoline_addr.as_u64());
        let mut gdt = x86_64::structures::gdt::GlobalDescriptorTable::new();
        {
            use x86_64::structures::gdt::DescriptorFlags;
            use x86_64::structures::gdt::Descriptor;

            let mut pm_code = DescriptorFlags::PRESENT | DescriptorFlags::EXECUTABLE | DescriptorFlags::GRANULARITY | DescriptorFlags::DEFAULT_SIZE | DescriptorFlags::USER_SEGMENT;
            let mut pm_data = DescriptorFlags::PRESENT | DescriptorFlags::GRANULARITY| DescriptorFlags::USER_SEGMENT | DescriptorFlags::DEFAULT_SIZE | DescriptorFlags::WRITABLE;
            // SAFETY: This is safe only bits 12:15 may be set in trampoline addr, left-shifted by 16 they are the base address 0:15 field
            let offset_code = unsafe { DescriptorFlags::from_bits_unchecked(trampoline_addr.as_u64() << 16) };
            let offset_data = unsafe { DescriptorFlags::from_bits_unchecked(( trampoline_addr.as_u64() +  (_trampoline_data.as_ptr() as usize as u64 - _trampoline as *const () as usize as u64) ) << 16) };

            pm_data |= offset_data;
            pm_code |= offset_code;

            // assertions are for my own sanity
            let pmc = gdt.add_entry(Descriptor::UserSegment(pm_code.bits())).0;
            let pmd = gdt.add_entry(Descriptor::UserSegment(pm_data.bits())).0;
            let kc = gdt.add_entry(Descriptor::kernel_code_segment()).0;
            let kd = gdt.add_entry(Descriptor::kernel_data_segment()).0;
            debug_assert_eq!(pmc, 8);
            debug_assert_eq!(pmd, 16);
            debug_assert_eq!(kc, 24);
            debug_assert_eq!(kd, 32);

        }

        let (l4_addr,_) = x86_64::registers::control::Cr3::read();
        let (addr,table) = if l4_addr.start_address().as_u64() > u32::MAX as u64 {
            let table = Box::new_in(crate::mem::SYS_MAPPER.get().get_l4_table().clone(),crate::alloc_interface::DmaAlloc::new(crate::mem::MemRegion::Mem16,4096));
            let addr = x86_64::PhysAddr::new(crate::mem::mem_map::translate_ptr(&*table).unwrap()); // no fuckin chance this panics
            (addr,Some(table))

        } else {
            (l4_addr.start_address(),None)
        };

        Self {
            gdt,
            l4_addr: addr,
            l4: table,
        }
    }

    fn get_or_new(opt: &mut Option<Self>, trampoline_addr: x86_64::VirtAddr) {
        if opt.is_none() {
            *opt = Some(Self::new(trampoline_addr));
        }
    }

}


fn long_mode_init() -> ! {
    use x86_64::{
        structures::gdt,
        registers::segmentation,
        registers::segmentation::Segment,
        registers::control::Cr0Flags,
    };

    unsafe fn cast_static<'a,T>(r: &'a T) -> &'static T {
        core::mem::transmute(r)
    }


    unsafe {core::arch::asm!("wbinvd")};
    // SAFETY: this enables cache, cache is invalidated above
    unsafe { x86_64::registers::control::Cr0::update(|f| f.set(Cr0Flags::CACHE_DISABLE | Cr0Flags::NOT_WRITE_THROUGH, false)); }
    unsafe { x86_64::registers::control::Cr0::update(|f| f.set(Cr0Flags::MONITOR_COPROCESSOR | Cr0Flags::NUMERIC_ERROR , true)); }

    // prevent the compiler from dropping the GDT and TSS because it must be present until the CPU is stopped.
    // ManuallyDrop keeps it static on the stack (because this frame is never dropped).
    let mut gdt = core::mem::ManuallyDrop::new(gdt::GlobalDescriptorTable::new());
    let c = gdt.add_entry(gdt::Descriptor::kernel_code_segment());
    let d = gdt.add_entry(gdt::Descriptor::kernel_data_segment());
    let tss = core::mem::ManuallyDrop::new(crate::gdt::new_tss());
    // SAFETY: tss is not dropped, before `run_exec()` therefore it is static enough.
    let tss_entry = gdt.add_entry(gdt::Descriptor::tss_segment(unsafe { &*(&*tss as *const x86_64::structures::tss::TaskStateSegment) }));

    // SAFETY: This points to a valid TSS and the data is valid
    unsafe {
        gdt::GlobalDescriptorTable::load(unsafe {cast_static(&gdt)});
        segmentation::CS::set_reg(c);
        segmentation::DS::set_reg(d);
        segmentation::SS::set_reg(d);
        let fsd = x86_msr::architecture::FsBase::read();
        segmentation::FS::set_reg(d);
        x86_msr::architecture::FsBase::write(fsd);
        x86_64::instructions::tables::load_tss(tss_entry);
    }

    crate::interrupts::load_idt();
    // todo lInt pins need to be configured
    crate::interrupts::apic::load_apic();

    // SAFETY: This is safe, all interrupts raised will be handled by existing IDT
    unsafe { crate::interrupts::apic::get_apic().set_enable(true) };

    // todo handle properly
    // this will require determining under what cases will the APIC timer not already be configured.
    crate::interrupts::apic::try_start_timer_residual().expect("Failed to start AP timer");


    crate::task::ap_setup_exec();
    ap_init_sync();
    crate::task::run_exec();
}

fn ap_init_sync() {
    AP_INIT_COUNT.fetch_sub(1,atomic::Ordering::Release);
    // busy loop while other CPUs are started
    while AP_INIT_COUNT.load(atomic::Ordering::Relaxed) != 0 {
        core::hint::spin_loop();
    }
}

#[repr(C)]
#[derive(Default)]
struct TransferSemaphore {
    data: core::sync::atomic::AtomicU64,
    request: atomic::Atomic<DataRequest>,
}

impl TransferSemaphore {
    /// Waits until the AP requests data returns the request ID. This fn will timeout after 2ms.
    fn wait(&self) -> Option<DataRequest> {
        //let timeout: crate::time::AbsoluteTime = crate::time::Duration::millis(2).into();
        loop {
            let rx = self.request.load(atomic::Ordering::Acquire);
            if rx == DataRequest::Empty {
                /*if !timeout.is_future() {
                    continue
                } else {
                    return None
                }
                 */
                continue
            } else {
                return Some(rx);
            }
        }
    }

    /// Responds to a request from the AP.
    /// This must be called as a response to [Self::wait] returning `Some(n)`
    fn send(&self, data: u64) {
        self.data.store(data,atomic::Ordering::Relaxed);
        self.request.store(DataRequest::Empty,atomic::Ordering::Release);
    }
}

/// Returns the diff between [_trampoline] and [_trampoline_data]
fn trampoline_data_diff() -> usize {
    // SAFETY: This is safe.
    // IMO this shouldn't need to be marked unsafe, im not actually accessing the value.
    unsafe { _trampoline_data.as_ptr() as *const () as usize - _trampoline as *const () as usize }
}

#[repr(u32)]
#[derive(Copy, Clone, Eq, PartialEq, Default)]
enum DataRequest {
    #[default]
    Empty = 0,
    PageAttributeTable = 1,
    TopLevelPageTable = 2,
    LongModeStack = 3,
    RustEntry = 4,
    KernelTLS = 5,
}

#[repr(C)]
struct TrampolineData {
    gdt_ptr: GdtPtr, // 0 size 3
    gdt: x86_64::structures::gdt::GlobalDescriptorTable, // 8 aligned up, size (8*9)=72
    xfer: TransferSemaphore, // 80 size 16
}

#[repr(packed,C)]
struct GdtPtr {
    size: u16,
    ptr: u32,
}

// imports
extern {
    fn _trampoline(); // this will never be directly called.
    static _trampoline_data: core::mem::MaybeUninit<TrampolineData>;
}

