#![no_std]
#![no_main]

extern crate alloc;

use kernel::gdt;
use kernel::process::task::MAX_TASKS;
use kernel::{MAX_CORES, boot_common, serial_println_core};
use x86_64::VirtAddr;
use x86_64::instructions::{hlt, port::Port};
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{OffsetPageTable, PageTable, Translate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    unsafe {
        let mut port = Port::new(0xF4);
        port.write(exit_code as u32);
    }
    loop {
        hlt();
    }
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    serial_println_core!("PANIC: {:#?}", info);
    exit_qemu(QemuExitCode::Failed);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kmain() -> ! {
    unsafe { boot_common::bsp_early_init() };
    test_main();
}

fn is_kernel_page_mapped(vaddr: VirtAddr) -> bool {
    let hhdm = kernel::boot_info::boot_info().hhdm_offset;
    let (pml4_frame, _) = Cr3::read();
    let pml4_virt = VirtAddr::new(pml4_frame.start_address().as_u64() + hhdm);
    let pml4_table = unsafe { &mut *pml4_virt.as_mut_ptr::<PageTable>() };
    let mapper = unsafe { OffsetPageTable::new(pml4_table, VirtAddr::new(hhdm)) };

    mapper.translate_addr(vaddr).is_some()
}

fn test_main() -> ! {
    serial_println_core!("=== test_guard_pages: start ===");
    serial_println_core!("MAX_CORES={}", MAX_CORES);

    let mut success = true;
    let mut total_checks: u32 = 0;
    let mut failed_checks: u32 = 0;

    for core_id in 0..MAX_CORES {
        let rsp0_guard = gdt::rsp0_guard_page(core_id);
        let ist0_guard = gdt::ist0_guard_page(core_id);
        let sched_guard = gdt::scheduler_guard_page(core_id);

        let (rsp0_bottom, rsp0_top) = gdt::rsp0_bounds(core_id);
        let (ist0_bottom, ist0_top) = gdt::ist0_bounds(core_id);
        let (sched_bottom, sched_top) = gdt::scheduler_bounds(core_id);

        // guard pages must be NOT mapped
        for (name, guard) in [
            ("RSP0", rsp0_guard),
            ("IST0", ist0_guard),
            ("SCHED", sched_guard),
        ] {
            total_checks += 1;
            let mapped = is_kernel_page_mapped(guard);
            if mapped {
                serial_println_core!(
                    "FAIL: core {} {} guard {:#x} should be UNMAPPED but is mapped",
                    core_id,
                    name,
                    guard.as_u64()
                );
                success = false;
                failed_checks += 1;
            } else {
                serial_println_core!(
                    "ok: core {} {} guard {:#x} unmapped",
                    core_id,
                    name,
                    guard.as_u64()
                );
            }
        }

        // stack bottom pages must be mapped
        for (name, bottom) in [
            ("RSP0", rsp0_bottom),
            ("IST0", ist0_bottom),
            ("SCHED", sched_bottom),
        ] {
            total_checks += 1;
            let mapped = is_kernel_page_mapped(bottom);
            if !mapped {
                serial_println_core!(
                    "FAIL: core {} {} stack bottom {:#x} should be MAPPED but is not",
                    core_id,
                    name,
                    bottom.as_u64()
                );
                success = false;
                failed_checks += 1;
            } else {
                serial_println_core!(
                    "ok: core {} {} stack bottom {:#x} mapped",
                    core_id,
                    name,
                    bottom.as_u64()
                );
            }
        }

        // stack top-1 page must be mapped
        for (name, top) in [("RSP0", rsp0_top), ("IST0", ist0_top), ("SCHED", sched_top)] {
            total_checks += 1;
            let last_byte = VirtAddr::new(top.as_u64() - 1);
            let mapped = is_kernel_page_mapped(last_byte);
            if !mapped {
                serial_println_core!(
                    "FAIL: core {} {} stack top-1 {:#x} should be MAPPED but is not",
                    core_id,
                    name,
                    last_byte.as_u64()
                );
                success = false;
                failed_checks += 1;
            } else {
                serial_println_core!(
                    "ok: core {} {} stack top-1 {:#x} mapped",
                    core_id,
                    name,
                    last_byte.as_u64()
                );
            }
        }

        // alignment checks: guard and stack bottom must be page aligned
        for (name, guard) in [
            ("RSP0", rsp0_guard),
            ("IST0", ist0_guard),
            ("SCHED", sched_guard),
        ] {
            total_checks += 1;
            if guard.as_u64() & 0xFFF != 0 {
                serial_println_core!(
                    "FAIL: core {} {} guard {:#x} not page aligned",
                    core_id,
                    name,
                    guard.as_u64()
                );
                success = false;
                failed_checks += 1;
            }
        }
        for (name, bottom) in [
            ("RSP0", rsp0_bottom),
            ("IST0", ist0_bottom),
            ("SCHED", sched_bottom),
        ] {
            total_checks += 1;
            if bottom.as_u64() & 0xFFF != 0 {
                serial_println_core!(
                    "FAIL: core {} {} bottom {:#x} not page aligned",
                    core_id,
                    name,
                    bottom.as_u64()
                );
                success = false;
                failed_checks += 1;
            }
        }
    }

    // every task kernel stack has an unmapped guard right below a mapped stack
    for slot_index in 0..MAX_TASKS {
        let guard = gdt::task_guard_page(slot_index);
        let (bottom, top) = gdt::task_stack_bounds(slot_index);
        let last_byte = VirtAddr::new(top.as_u64() - 1);

        total_checks += 1;
        if is_kernel_page_mapped(guard) {
            serial_println_core!(
                "FAIL: task slot {} guard {:#x} should be UNMAPPED but is mapped",
                slot_index,
                guard.as_u64()
            );
            success = false;
            failed_checks += 1;
        }

        total_checks += 1;
        if !is_kernel_page_mapped(bottom) {
            serial_println_core!(
                "FAIL: task slot {} stack bottom {:#x} should be MAPPED but is not",
                slot_index,
                bottom.as_u64()
            );
            success = false;
            failed_checks += 1;
        }

        total_checks += 1;
        if !is_kernel_page_mapped(last_byte) {
            serial_println_core!(
                "FAIL: task slot {} stack top-1 {:#x} should be MAPPED but is not",
                slot_index,
                last_byte.as_u64()
            );
            success = false;
            failed_checks += 1;
        }

        total_checks += 1;
        if guard.as_u64() & 0xFFF != 0 || bottom.as_u64() & 0xFFF != 0 {
            serial_println_core!(
                "FAIL: task slot {} guard or bottom not page aligned",
                slot_index
            );
            success = false;
            failed_checks += 1;
        }

        total_checks += 1;
        if guard + 4096u64 != bottom {
            serial_println_core!(
                "FAIL: task slot {} guard {:#x} +4096 != bottom {:#x}",
                slot_index,
                guard.as_u64(),
                bottom.as_u64()
            );
            success = false;
            failed_checks += 1;
        }

        total_checks += 1;
        if top.as_u64() % 16 != 0 {
            serial_println_core!(
                "FAIL: task slot {} stack top not 16 byte aligned",
                slot_index
            );
            success = false;
            failed_checks += 1;
        }
    }

    // also verify that guard page is exactly one page below stack bottom
    for core_id in 0..MAX_CORES {
        let rsp0_guard = gdt::rsp0_guard_page(core_id);
        let (rsp0_bottom, _) = gdt::rsp0_bounds(core_id);
        total_checks += 1;
        if rsp0_guard + 4096u64 != rsp0_bottom {
            serial_println_core!(
                "FAIL: core {} RSP0 guard {:#x} +4096 != bottom {:#x}",
                core_id,
                rsp0_guard.as_u64(),
                rsp0_bottom.as_u64()
            );
            success = false;
            failed_checks += 1;
        }
    }

    serial_println_core!(
        "test_guard_pages: {}/{} checks passed",
        total_checks - failed_checks,
        total_checks
    );

    if success {
        serial_println_core!("PASS: all guard pages correctly installed");
        serial_println_core!("=== test_guard_pages: ok ===");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println_core!("FAIL: {} checks failed", failed_checks);
        serial_println_core!("=== test_guard_pages: FAILED ===");
        exit_qemu(QemuExitCode::Failed);
    }
}
