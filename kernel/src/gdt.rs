use lazy_static::lazy_static;
use x86_64::instructions::segmentation::SS;
use x86_64::registers::segmentation::FS;
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
const STACK_SIZE: usize = 4096 * 5;

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

            // SAFETY: This is safe STACK is only accessed by the bsp
            #[allow(unused_unsafe)] // IMO this should have unsafe {}
            let stack_start = VirtAddr::from_ptr(&raw const STACK);
            let stack_end = stack_start + STACK_SIZE as u64;
            stack_end
        };
        tss
    };
}

use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let tss_selector = gdt.append(Descriptor::tss_segment(&TSS));

        //needed after bootloader 0.10
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        (
            gdt,
            Selectors {
                code_selector,
                data_selector,
                tss_selector,
            },
        )
    };
}

pub(crate) fn new_tss() -> TaskStateSegment {
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
        // This is preferred over allocating a Box<[_,_]>
        let mut b: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
        b.resize(STACK_SIZE, 0);
        let l = b.leak();
        VirtAddr::from_ptr(&l[0]) + STACK_SIZE as u64
    };
    tss
}

struct Selectors {
    code_selector: SegmentSelector,
    tss_selector: SegmentSelector,
    data_selector: SegmentSelector,
}

pub fn init() {
    use x86_64::instructions::segmentation::Segment;
    use x86_64::instructions::segmentation::CS;
    use x86_64::instructions::tables::load_tss;
    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        load_tss(GDT.1.tss_selector);
        SS::set_reg(GDT.1.data_selector);
        FS::set_reg(GDT.1.data_selector); // Set base addr using `IA32_FS_BASE`.
    }
}
