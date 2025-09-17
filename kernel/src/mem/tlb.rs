//! TLB entries are not coherent with other CPUs or system memory.
//! For this reason they must be invalidated by software.
//!
//! The purpose of this module is to maintain coherency by determining when TLB entries need to be invalidated.
//! This is done by performing TLB shootdowns. Shootdowns are expensive because they
//! require raising an interrupt on one or several other processors and waiting until all other
//! CPUs have invalidated the requested TLB entries.
//!
//! Hootux' model for handling TLB synchronization is to always raise a shootdown when
//! page table entries are modified (excl setting  "P" flag).
//! Modules may implement their own TLB synchronization and use the [without_shootdowns] fn to improve performance

use crate::interrupts::apic;
use crate::interrupts::apic::Apic;
use core::ops::Deref;

const SHOOTDOWN_VECTOR: crate::interrupts::InterruptIndex =
    crate::interrupts::InterruptIndex::TlbShootdown; // This uses a u8 because it needs to use a vector not an IRQ. This will use a private kernel API
static SHOOTDOWN_SYNC: ShootdownSyncCounter = ShootdownSyncCounter::new();
#[thread_local]
static MASK_SHOOTDOWN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true); // default state should be true on bsp and false on ap
static SHOOTDOWN_LIST: ShootdownListMutex = ShootdownListMutex::new();
static SHOOTDOWN_WARN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// This fn is to indicate that page tables are being modified and a shootdown is about to occur.
/// This will indicate to other CPUs that page faults may occur and should be retried.
pub fn shootdown_hint<T, F>(f: F) -> T
where
    F: FnOnce() -> T,
{
    SHOOTDOWN_WARN.fetch_add(1, atomic::Ordering::Acquire);
    let rc = f();
    SHOOTDOWN_WARN.fetch_sub(1, atomic::Ordering::Release);
    rc
}

/// Initiates a TLB shootdown to all CPUs on the memory regions provided in `shootdown_content`.
/// This should only be used to shootdown kernel memory regions.
pub fn shootdown(shootdown_content: ShootdownContent) {
    if MASK_SHOOTDOWN.load(atomic::Ordering::Relaxed) {
        return;
    }
    let _l = SHOOTDOWN_LIST.set(shootdown_content); // not used, dropped at the end of this scope
    SHOOTDOWN_SYNC.sync(crate::mp::num_cpus(), || {
        unsafe {
            apic::get_apic()
                .send_ipi(
                    apic::IpiTarget::AllNotThisCpu,
                    apic::InterruptType::Fixed,
                    SHOOTDOWN_VECTOR.as_u8(),
                )
                .unwrap()
        };
        handle_shootdown(); // handle shootdown here while we're waiting for the others.
    });
}

/// Performs a targeted shootdown to specified CPUs.
/// This has a lesser performance impact than [shootdown],
/// however should only be used when its known which CPUs have cached `shootdown_content`
///
/// Note that the CPU may speculatively cache `shootdown_content`.
pub fn target_shootdown(targets: crate::mp::bitmap::CpuBMap, shootdown_content: ShootdownContent) {
    if MASK_SHOOTDOWN.load(atomic::Ordering::Relaxed) {
        return;
    }
    let c = if let Some(c) = targets.count() {
        c
    } else {
        return;
    };

    let tgt_self = if targets.get(crate::mp::who_am_i()) {
        targets.clear();
        true
    } else {
        false
    };

    let _l = SHOOTDOWN_LIST.set(shootdown_content);
    SHOOTDOWN_SYNC.sync(c, || {
        for i in targets.iter() {
            // SAFETY: The actions of the other CPU are
            unsafe {
                apic::get_apic()
                    .send_ipi(
                        apic::IpiTarget::Other(i),
                        apic::InterruptType::Fixed,
                        SHOOTDOWN_VECTOR.as_u8(),
                    )
                    .unwrap()
            }
        }
        if tgt_self {
            handle_shootdown();
        }
    });
}

struct ShootdownSyncCounter {
    lock: core::sync::atomic::AtomicBool,
    count: atomic::Atomic<crate::mp::CpuCount>,
}

impl ShootdownSyncCounter {
    const fn new() -> Self {
        Self {
            lock: core::sync::atomic::AtomicBool::new(false),
            count: atomic::Atomic::new(0),
        }
    }

    /// Syncs all other CPUs to this one.
    ///
    /// This fn will acquire an internal spinlock, once it is acquired `f` is called. `f`
    /// must ensure that [self.shoot] is called `count` times.
    /// This fn will spin until [self.shoot] has been called `count` times.
    fn sync<T, F>(&self, count: crate::mp::CpuCount, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        loop {
            match self.lock.compare_exchange_weak(
                false,
                true,
                atomic::Ordering::Acquire,
                atomic::Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.count.store(count, atomic::Ordering::Release);
                    let rc = f();
                    while self.count.load(atomic::Ordering::Relaxed) != 0 {
                        core::hint::spin_loop();
                    }
                    self.lock.store(false, atomic::Ordering::Release);
                    return rc;
                }
                Err(_) => {
                    core::hint::spin_loop();
                    continue;
                }
            }
        }
    }

    fn shoot(&self) {
        self.count.fetch_sub(1, atomic::Ordering::Release);
    }
}

/// Blocks TLB-shootdowns from occurring within `f`.
///
/// # Safety
///
/// It should be obvious that TLB coherency must be synchronized by `f`.
/// This does not require a shootdown to be emitted by `f`.
/// TLBs can remain coherent as long as a CPU never access memory using older TLB entries even of the old entries remain.
pub unsafe fn without_shootdowns<T, F>(f: F) -> T
where
    F: FnOnce() -> T,
{
    let state = MASK_SHOOTDOWN.swap(true, atomic::Ordering::Relaxed);
    let r = f();
    MASK_SHOOTDOWN.store(state, atomic::Ordering::Release);
    r
}

pub(crate) fn enable_shootdowns() {
    MASK_SHOOTDOWN.store(false, atomic::Ordering::Release);
}

fn handle_shootdown() {
    // SAFETY: This interrupt is handled properly, it is edge-triggered so EOI can be sent before handling.
    unsafe { apic::apic_eoi() }
    // this might seem a bit misleading
    SHOOTDOWN_LIST.visit().unwrap().invalidate();
    SHOOTDOWN_SYNC.shoot();
}

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        #[doc(hidden)]
        pub(crate) extern "x86-interrupt" fn int_shootdown_wrapper(_sf: x86_64::structures::idt::InterruptStackFrame) {
           handle_shootdown()
        }
    } else {
        compile_error!("Arch ot supported");
    }
}

/// This is an odd mutex, it must be locked by a writer for the data to be read.
/// ```ignore
/// fn example(data: alloc::vec::Vec<CompressedTlbEntry>) {
///     let m = ShootdownListMutex::new();
///     let l = m.set();
///     let r = m.get();
///     for i in r {
///         // do things
///     }
///     drop(r);
///     drop(l); // this will deadlock if r is not dropped.
///     assert_eq!(m.get(),None) // this will panic, because `m` contains no data
///
/// }
// todo should this be moved to hootux::util::mutex
struct ShootdownListMutex {
    master: atomic::Atomic<bool>,
    guest: atomic::Atomic<usize>,
    data: core::cell::UnsafeCell<ShootdownContent>,
}

// SAFETY: This contains its own synchronization
unsafe impl Sync for ShootdownListMutex {}
unsafe impl Send for ShootdownListMutex {}

struct ShootdownMutexMasterGuard<'a> {
    parent: &'a ShootdownListMutex,
}

impl<'a> Drop for ShootdownMutexMasterGuard<'a> {
    fn drop(&mut self) {
        // master can leave while guests are present.
        // spin until they leave.
        while self.parent.guest.load(atomic::Ordering::Relaxed) > 1 {
            core::hint::spin_loop();
        }
        self.parent.guest.fetch_sub(1, atomic::Ordering::Release);
        *unsafe { &mut *self.parent.data.get() } = ShootdownContent::None;
        self.parent.master.store(false, atomic::Ordering::Release);
    }
}

impl ShootdownListMutex {
    const fn new() -> Self {
        Self {
            master: atomic::Atomic::new(false),
            guest: atomic::Atomic::new(0),
            data: core::cell::UnsafeCell::new(ShootdownContent::None),
        }
    }
    fn set(&self, data: ShootdownContent) -> ShootdownMutexMasterGuard<'_> {
        while let Err(_) = self.master.compare_exchange_weak(
            false,
            true,
            atomic::Ordering::Acquire,
            atomic::Ordering::Relaxed,
        ) {
            core::hint::spin_loop();
        }
        unsafe { self.data.get().write(data) }
        self.guest.fetch_add(1, atomic::Ordering::Acquire);
        ShootdownMutexMasterGuard { parent: self }
    }

    fn visit(&self) -> Option<ShootdownVisitorGuard<'_>> {
        if self.master.load(atomic::Ordering::Acquire) {
            // Either data is bing dropped or initialized if visitors is 0.
            // Loop until it is >1
            let mut m = self.master.load(atomic::Ordering::Acquire);
            while m && self.guest.load(atomic::Ordering::Acquire) == 0 {
                // self was released, there is no data to acquire
                if m == false {
                    return None;
                }
                core::hint::spin_loop();
                m = self.master.load(atomic::Ordering::Acquire);
            }
            self.guest.fetch_add(1, atomic::Ordering::Acquire);
            Some(ShootdownVisitorGuard { parent: self })
        } else {
            None
        }
    }
}

struct ShootdownVisitorGuard<'a> {
    parent: &'a ShootdownListMutex,
}

impl<'a> Drop for ShootdownVisitorGuard<'a> {
    fn drop(&mut self) {
        self.parent.guest.fetch_sub(1, atomic::Ordering::Release);
    }
}

impl<'a> Deref for ShootdownVisitorGuard<'a> {
    type Target = ShootdownContent;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.parent.data.get() }
    }
}

/// Compressed version of [TlbDropEntry] using only 8bytes. The len of the compressed value has
/// a limit depending on the page size it represents.
// The count is stored as count - 1. So a value of 0 is a count of 1
#[cfg(target_arch = "x86_64")]
#[derive(Debug, Copy, Clone)]
pub struct CompressedTlbDropEntry {
    raw: u64,
}

/// Represents a region of memory to be flushed from the TLB.
///
/// This implements both [From<CompressedTlbDropEntry>] and [TryInto<CompressedTlbDropEntry>].
/// If `TryInto` returns `Err(_)` the returned value contains a valid
/// `CompressedTlbDropEntry` and `CompressedTlbDropEntry` that represents as much of the area
/// as possible try_into may be called on the `Err()` value until `Ok()` is returned
///
/// Shootdowns may need to be performed on multiple regions of memory, so [Self] may need to be
/// stored in a [alloc::vec::Vec], however storing more than 1028 entries may require another
/// shootdown, [CompressedTlbEntry] is provided to decrease the amount of heap memory required
/// to perform a shootdown
#[cfg(target_arch = "x86_64")]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum TlbDropEntry {
    Kib4 {
        addr: x86_64::structures::paging::Page<x86_64::structures::paging::Size4KiB>,
        count: u32,
    },
    Mib2 {
        addr: x86_64::structures::paging::Page<x86_64::structures::paging::Size4KiB>,
        count: u32,
    },
    Gib1 {
        addr: x86_64::structures::paging::Page<x86_64::structures::paging::Size4KiB>,
        count: u32,
    },
}

impl TlbDropEntry {
    fn flush(self) {
        match self {
            TlbDropEntry::Kib4 { mut addr, count } => {
                for _ in 0..count {
                    x86_64::instructions::tlb::flush(addr.start_address());
                    addr += 1;
                }
            }
            TlbDropEntry::Mib2 { mut addr, count } => {
                for _ in 0..count {
                    x86_64::instructions::tlb::flush(addr.start_address());
                    addr += 1;
                }
            }
            TlbDropEntry::Gib1 { mut addr, count } => {
                for _ in 0..count {
                    x86_64::instructions::tlb::flush(addr.start_address());
                    addr += 1;
                }
            }
        }
    }

    /// Attempts to append `other` to `self`.
    /// To succeed both must be the same variant, and `self.count` must not overflow.
    pub fn append(&mut self, other: Self) -> Option<Self> {
        match (self, other) {
            (
                TlbDropEntry::Kib4 { addr, count },
                TlbDropEntry::Kib4 {
                    addr: op,
                    count: oc,
                },
            ) => {
                if *addr + *count as u64 + 1 == op {
                    *count = count.checked_add(oc)?
                }
            }
            (
                TlbDropEntry::Mib2 { addr, count },
                TlbDropEntry::Mib2 {
                    addr: op,
                    count: oc,
                },
            ) => {
                if *addr + *count as u64 + 1 == op {
                    *count = count.checked_add(oc)?
                }
            }
            (
                TlbDropEntry::Gib1 { addr, count },
                TlbDropEntry::Gib1 {
                    addr: op,
                    count: oc,
                },
            ) => {
                if *addr + *count as u64 + 1 == op {
                    *count = count.checked_add(oc)?
                }
            }
            _ => {}
        }
        None
    }
}

impl From<CompressedTlbDropEntry> for TlbDropEntry {
    fn from(value: CompressedTlbDropEntry) -> Self {
        match value.raw & 0b11 {
            0 => {
                // SAFETY: This is safe because the address is always 4K aligned
                let page = unsafe {
                    x86_64::structures::paging::Page::from_start_address_unchecked(
                        x86_64::VirtAddr::new(value.raw & !((1 << 13) - 1)),
                    )
                };
                let count = ((value.raw & ((1 << 11) - 1) << 2) >> 2) as u32;
                Self::Kib4 { addr: page, count }
            }
            1 => {
                let page = unsafe {
                    x86_64::structures::paging::Page::from_start_address_unchecked(
                        x86_64::VirtAddr::new(value.raw & !((1 << 22) - 1)),
                    )
                };
                let count = ((value.raw & ((1 << 20) - 1) << 2) >> 2) as u32;
                Self::Mib2 { addr: page, count }
            }
            2 => {
                let page = unsafe {
                    x86_64::structures::paging::Page::from_start_address_unchecked(
                        x86_64::VirtAddr::new(value.raw & !((1 << 31) - 1)),
                    )
                };
                let count = ((value.raw & ((1 << 29) - 1) << 2) >> 2) as u32;
                Self::Gib1 { addr: page, count }
            }
            3 => panic!(
                "{} Failed to read {value:x?}",
                core::any::type_name::<Self>()
            ),

            // SAFETY: This is safe matched value is `& 3` only 0..=3 values are possible
            _ => unsafe { core::hint::unreachable_unchecked() },
        }
    }
}

impl<S: x86_64::structures::paging::PageSize + 'static> From<x86_64::structures::paging::Page<S>>
    for TlbDropEntry
{
    fn from(value: x86_64::structures::paging::Page<S>) -> Self {
        use core::any::TypeId;
        // SAFETY: These unchecked Page constructors ar basically just typecasting from Page<T> into Page<Size#>
        // Comparisons are evaluated at compile time
        if TypeId::of::<S>() == TypeId::of::<x86_64::structures::paging::Size4KiB>() {
            Self::Kib4 {
                addr: unsafe {
                    x86_64::structures::paging::Page::from_start_address_unchecked(
                        value.start_address(),
                    )
                },
                count: 1,
            }
        } else if TypeId::of::<S>() == TypeId::of::<x86_64::structures::paging::Size2MiB>() {
            Self::Mib2 {
                addr: unsafe {
                    x86_64::structures::paging::Page::from_start_address_unchecked(
                        value.start_address(),
                    )
                },
                count: 1,
            }
        } else if TypeId::of::<S>() == TypeId::of::<x86_64::structures::paging::Size1GiB>() {
            Self::Gib1 {
                addr: unsafe {
                    x86_64::structures::paging::Page::from_start_address_unchecked(
                        value.start_address(),
                    )
                },
                count: 1,
            }
        } else {
            // This will be dropped by the linker
            unreachable!()
        }
    }
}

impl TryInto<CompressedTlbDropEntry> for TlbDropEntry {
    type Error = (CompressedTlbDropEntry, Self);
    fn try_into(self) -> Result<CompressedTlbDropEntry, Self::Error> {
        match self {
            TlbDropEntry::Kib4 { addr, count } => {
                let mut raw = addr.start_address().as_u64();
                if count > (0x1000 >> 2) {
                    Err((
                        CompressedTlbDropEntry { raw },
                        Self::Kib4 {
                            addr: addr + (0x1000 >> 2),
                            count: count + 0x1000 >> 2,
                        },
                    ))
                } else {
                    raw &= (count as u64 - 1) << 2;
                    Ok(CompressedTlbDropEntry { raw })
                }
            }
            TlbDropEntry::Mib2 { addr, count } => {
                let mut raw = addr.start_address().as_u64();
                if count > (0x200000 >> 2) {
                    Err((
                        CompressedTlbDropEntry { raw },
                        Self::Kib4 {
                            addr: addr + (0x200000u64 >> 2),
                            count: count + 0x1000 >> 2,
                        },
                    ))
                } else {
                    raw &= (count as u64 - 1) << 2;

                    Ok(CompressedTlbDropEntry { raw })
                }
            }
            TlbDropEntry::Gib1 { addr, count } => {
                let mut raw = addr.start_address().as_u64();
                if count > (0x10000000 >> 2) {
                    // I'm not entirely sure if this branch is possible
                    Err((
                        CompressedTlbDropEntry { raw },
                        Self::Kib4 {
                            addr: addr + (0x10000000u64 >> 2),
                            count: count + 0x1000 >> 2,
                        },
                    ))
                } else {
                    raw &= (count as u64 - 1) << 2;
                    Ok(CompressedTlbDropEntry { raw })
                }
            }
        }
    }
}

#[non_exhaustive]
pub enum ShootdownContent {
    /// Do nothing.
    None,
    /// Drop a single memory region
    Short(TlbDropEntry),
    /// Invalidate multiple memory regions
    ///
    /// If the vec is longer than 256 entries then dropping `self` may raise another shootdown.
    Long(alloc::vec::Vec<CompressedTlbDropEntry>),
    /// Invalidate the entire context, excl global pages
    FullContext,
    // Invalidate the kernel region.
    // Kernel
}

impl ShootdownContent {
    /// Invalidates TLB entries which `self` represents.
    fn invalidate(&self) {
        match self {
            ShootdownContent::None => {} // do nothing
            ShootdownContent::Short(t) => t.flush(),
            ShootdownContent::Long(l) => {
                for i in l {
                    let u: TlbDropEntry = (*i).into();
                    u.flush();
                }
            }
            ShootdownContent::FullContext => {
                cfg_if::cfg_if!(
                    if #[cfg(target_arch = "x86_64")] {
                        // SAFETY: This invalidates all TLBs except global ones it does not introduce UB
                        unsafe {
                            core::arch::asm!(
                                "mov {0},cr3",
                                "mov cr3,{0}",
                                out(reg) _
                            );
                        };
                    } else {
                        compiler_error!("Not supported");
                    }
                );
            }
        }
    }
}

impl<S: x86_64::structures::paging::PageSize + 'static> From<x86_64::structures::paging::Page<S>>
    for ShootdownContent
{
    fn from(value: x86_64::structures::paging::Page<S>) -> Self {
        Self::Short(value.into())
    }
}

/*
impl<T,U> From<T> for ShootdownContent
    where
        T: Iterator<Item = U>,
        U: Into<TlbDropEntry>
{
    fn from(value: T) -> Self {
        let mut v = alloc::vec::Vec::new();
        let mut hold = None;

        for i in value.map(|i| i.into()) {
            if hold == None {
                hold = Some(i);
            } else {
                if let Some(mut of) = hold.as_mut().unwrap().append(i) {
                    loop {
                        match of.try_into() {
                            Ok(comp) => {
                                v.push(comp);
                                break;
                            },
                            Err((comp,oof)) => {
                                v.push(comp);
                                // do it again.
                                of = oof;
                            }
                        }
                    }
                }
            }
        }

        loop {
            match hold.map(|i| i.try_into()) {
                Some(Ok(comp)) => {
                    v.push(comp);
                    break;
                },
                Some(Err((comp, of))) => {
                    v.push(comp);
                    // do it again.
                    hold = Some(of);
                }
                None => {break}
            }
        }

        if v.len() > 0 {
            Self::Long(v)
        } else {
            Self::Short(hold.unwrap()) // if v.len() is 0 then this cannot be None
        }
    }
}

 */
