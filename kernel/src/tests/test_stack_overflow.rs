#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    LIMINE_BASE_REVISION, gdt, serial_print,
    testing::{QemuExitCode, exit_qemu, test_panic_handler},
};
use lazy_static::lazy_static;
use limine::{BaseRevision, RequestsEndMarker, RequestsStartMarker};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START: RequestsStartMarker = RequestsStartMarker::new();
#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::with_revision(LIMINE_BASE_REVISION);
#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END: RequestsEndMarker = RequestsEndMarker::new();

lazy_static! {
    static ref TEST_IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        unsafe {
            idt.double_fault
                .set_handler_fn(test_double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt
    };
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    assert!(BASE_REVISION.is_supported());
    serial_print!("test_stack_overflow::stack_overflow...\t");
    gdt::init();
    TEST_IDT.load();
    stack_overflow();
    panic!("execution continued after stack overflow");
}

#[allow(unconditional_recursion)]
fn stack_overflow() {
    stack_overflow();
    core::hint::black_box(());
}

extern "x86-interrupt" fn test_double_fault_handler(
    _frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    // TODO: investigate why printing someting here sometimes causes a triple fault instead of a clean exit
    exit_qemu(QemuExitCode::Success)
}
