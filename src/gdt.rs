use lazy_static::lazy_static;
use x86_64::VirtAddr;
use x86_64::instructions::tables::load_tss;
use x86_64::registers::segmentation::{CS, Segment};
use x86_64::structures::gdt::SegmentSelector;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();

        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096 * 5;
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

            let stack_start = VirtAddr::from_ptr(&raw const STACK);

            stack_start + STACK_SIZE as u64
        };

        tss
    };
}

struct Gdt {
    table: GlobalDescriptorTable,
    selectors: Selectors,
}

lazy_static! {
    static ref GDT: Gdt = {
        let mut table = GlobalDescriptorTable::new();
        let code_selector = table.append(Descriptor::kernel_code_segment());
        let tss_selector = table.append(Descriptor::tss_segment(&TSS));
        Gdt {
            table,
            selectors: Selectors {
                code_selector,
                tss_selector,
            },
        }
    };
}

struct Selectors {
    code_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

pub fn init() {
    GDT.table.load();
    // SAFETY: We have just loaded the GDT, so the selectors are valid.
    unsafe {
        CS::set_reg(GDT.selectors.code_selector);
        load_tss(GDT.selectors.tss_selector);
    }
}
