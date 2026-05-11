use crate::serial_println;
use lazy_static::lazy_static;
use x86_64::{
    VirtAddr,
    instructions::tables::load_tss,
    registers::segmentation::{CS, DS, ES, SS, Segment},
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector},
        tss::TaskStateSegment,
    },
};

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        const STACK_SIZE: usize = 4 * 1024 * 1024;

        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

            let stack_start = VirtAddr::from_ptr(&raw const STACK);

            stack_start + STACK_SIZE as u64
        };

        let val = tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize];
        serial_println!(
            "Initializing TSS: interrupt_stack_table[double_fault]: {:#x}",
            val
        );

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
        let kernel_code_selector = table.append(Descriptor::kernel_code_segment());
        let kernel_data_selector = table.append(Descriptor::kernel_data_segment());

        let user_code_selector = table.append(Descriptor::user_code_segment());
        let user_data_selector = table.append(Descriptor::user_data_segment());

        let tss_selector = table.append(Descriptor::tss_segment(&TSS));

        Gdt {
            table,
            selectors: Selectors {
                kernel_code_selector,
                kernel_data_selector,
                user_code_selector,
                user_data_selector,
                tss_selector,
            },
        }
    };
}

struct Selectors {
    kernel_code_selector: SegmentSelector,
    kernel_data_selector: SegmentSelector,
    user_code_selector: SegmentSelector,
    user_data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

pub fn init() {
    serial_println!("Initializing GDT");
    GDT.table.load();
    // SAFETY: We have just loaded the GDT, so the selectors are valid.
    unsafe {
        CS::set_reg(GDT.selectors.kernel_code_selector);
        SS::set_reg(GDT.selectors.kernel_data_selector);
        DS::set_reg(GDT.selectors.kernel_data_selector);
        ES::set_reg(GDT.selectors.kernel_data_selector);
        load_tss(GDT.selectors.tss_selector);
    }

    serial_println!("GDT initialized:");
    serial_println!(
        "  Kernel code selector: {:?}",
        GDT.selectors.kernel_code_selector
    );
    serial_println!(
        "  Kernel data selector: {:?}",
        GDT.selectors.kernel_data_selector
    );
    serial_println!(
        "  User code selector: {:?}",
        GDT.selectors.user_code_selector
    );
    serial_println!(
        "  User data selector: {:?}",
        GDT.selectors.user_data_selector
    );
    serial_println!("  TSS selector: {:?}", GDT.selectors.tss_selector);
}

pub fn get_user_code_selector() -> SegmentSelector {
    GDT.selectors.user_code_selector
}

pub fn get_user_data_selector() -> SegmentSelector {
    GDT.selectors.user_data_selector
}

pub fn get_kernel_code_selector() -> SegmentSelector {
    GDT.selectors.kernel_code_selector
}

pub fn get_kernel_data_selector() -> SegmentSelector {
    GDT.selectors.kernel_data_selector
}
