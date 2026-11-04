#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    gdt, serial_print,
    testing::{QemuExitCode, exit_qemu, test_panic_handler},
};
use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

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
    // bsp_early_init needs more than the 32 KiB RSP0 stack, so it runs on the
    // bootloader stack like the real kernel's kmain.
    unsafe { kernel::boot_common::bsp_early_init() };
    unsafe {
        let top = kernel::gdt::rsp0_stack_top(0);

        core::arch::asm!(
            "mov rsp, {top}",
            "call {inner}",
            top = in(reg) top,
            inner = sym test_inner,
            options(noreturn)
        );
    }
}

extern "C" fn test_inner() -> ! {
    serial_print!("test_stack_overflow::stack_overflow...\t");

    {
        let rsp: u64;

        unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack)) };

        let (bottom, top) = gdt::rsp0_bounds(0);
        let (ist_bottom, ist_top) = gdt::ist0_bounds(0);
        kernel::serial_println!(
            "RSP={:#x} RSP0=[{:#x}..{:#x}] IST0=[{:#x}..{:#x}]",
            rsp,
            bottom.as_u64(),
            top.as_u64(),
            ist_bottom.as_u64(),
            ist_top.as_u64()
        );

        debug_assert!(
            rsp >= bottom.as_u64() && rsp <= top.as_u64(),
            "RSP not in RSP0"
        );
    }

    x86_64::instructions::interrupts::disable();
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
