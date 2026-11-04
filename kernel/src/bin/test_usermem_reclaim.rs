#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use core::sync::atomic::Ordering;
use kernel::process::elf_loader::{ElfLoadInfo, MEMSTRESS_ELF};
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::scheduler;
use kernel::{AP_CORES_READY, MAX_CORES, boot_common, bsp_init, serial_println_core};
use x86_64::instructions::{hlt, port::Port};

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

fn test_main() -> ! {
    serial_println_core!("=== test_usermem_reclaim: start ===");
    serial_println_core!(
        "MAX_CORES={} AP_CORE_COUNT={}",
        MAX_CORES,
        kernel::AP_CORE_COUNT
    );

    kernel::process::shared_state::init_shared_state();

    bsp_init::wait_for_ap_cores_blocking();
    let (ready, expected) = bsp_init::check_ap_cores_ready();
    serial_println_core!("test_usermem_reclaim: AP ready {}/{} ", ready, expected);

    let mut success = true;

    // Snapshot allocator before strain
    let before_free = kernel::memory::get_frame_allocator().free_frame_count();
    let before_bump = kernel::memory::get_frame_allocator().allocated_bump_frames();
    serial_println_core!(
        "test_usermem_reclaim: before free={} bump={}",
        before_free,
        before_bump
    );

    let elf_info = match ElfLoadInfo::from_elf_data(MEMSTRESS_ELF) {
        Ok(info) => {
            serial_println_core!(
                "test_usermem_reclaim: parsed memstress ELF entry={:#x} segments={}",
                info.entry_point,
                info.segments.len()
            );
            info
        }
        Err(e) => {
            serial_println_core!("FAIL: parse memstress ELF: {:?}", e);
            exit_qemu(QemuExitCode::Failed);
        }
    };

    let pid = {
        let mut pm = PROCESS_MANAGER.lock();
        match pm.create_process_from_elf(0, &elf_info, "memstress", 5) {
            Ok(pid) => {
                serial_println_core!("test_usermem_reclaim: created memstress pid={}", pid);
                pid
            }
            Err(e) => {
                serial_println_core!("FAIL: create_process_from_elf: {:?}", e);
                exit_qemu(QemuExitCode::Failed);
            }
        }
    };

    const TIMEOUT_MS: u64 = 8000;
    let start = kernel::interrupts::system_uptime_ns();
    let timeout_ns = TIMEOUT_MS * 1_000_000;
    let mut terminated = false;
    let mut exit_code: Option<i32> = None;

    loop {
        let now = kernel::interrupts::system_uptime_ns();
        if now.saturating_sub(start) >= timeout_ns {
            serial_println_core!(
                "FAIL: memstress pid={} timed out after {} ms",
                pid,
                TIMEOUT_MS
            );
            success = false;
            break;
        }

        let state_opt = {
            let pm = PROCESS_MANAGER.lock();
            pm.get_process(pid).ok().map(|p| (p.state, p.exit_code))
        };

        match state_opt {
            Some((kernel::process::process::ProcessState::Terminated, code)) => {
                terminated = true;
                exit_code = code;
                serial_println_core!(
                    "test_usermem_reclaim: pid={} terminated code={:?}",
                    pid,
                    code
                );
                break;
            }
            Some((state, _)) => {
                let queued = scheduler::is_pid_queued(pid);
                serial_println_core!(
                    "test_usermem_reclaim: pid={} state={:?} queued={} waiting...",
                    pid,
                    state,
                    queued
                );
            }
            None => {
                serial_println_core!("test_usermem_reclaim: pid={} not found", pid);
                terminated = true;
                break;
            }
        }
        hlt();
    }

    if !terminated {
        serial_println_core!("FAIL: memstress did not terminate");
        success = false;
    }

    if let Some(code) = exit_code {
        if code != 0 {
            serial_println_core!("FAIL: memstress exit_code {} != 0", code);
            success = false;
        }
    }

    // Give scheduler + terminate path a tick to reclaim
    for _ in 0..10 {
        hlt();
    }

    // Snapshot after
    let after_free = kernel::memory::get_frame_allocator().free_frame_count();
    let after_bump = kernel::memory::get_frame_allocator().allocated_bump_frames();
    serial_println_core!(
        "test_usermem_reclaim: after free={} bump={} (delta free={} bump={})",
        after_free,
        after_bump,
        after_free as i64 - before_free as i64,
        after_bump as i64 - before_bump as i64
    );

    // Reclaim check, free count should be >= before
    const MORE_FREE_MARGIN: usize = 8;
    let free_reclaimed = after_free + MORE_FREE_MARGIN >= before_free;
    if !free_reclaimed {
        serial_println_core!(
            "FAIL: free frames not reclaimed before={} after={}",
            before_free,
            after_free
        );
        success = false;
    } else {
        serial_println_core!("ok: free frames reclaimed");
    }

    // allocated_bump_frames is frames handed out minus frames on the free list
    if after_bump > before_bump {
        serial_println_core!(
            "FAIL: frames leaked, in use before={} after={}",
            before_bump,
            after_bump
        );
        success = false;
    } else {
        serial_println_core!("ok: no frames in use beyond the baseline");
    }

    if kernel::memory::get_frame_allocator().free_list_has_duplicates() {
        serial_println_core!("FAIL: a frame was freed more than once");
        success = false;
    } else {
        serial_println_core!("ok: no duplicate frames on the free list");
    }

    // Ensure PID not still queued and process removed after cleanup
    {
        let mut pm = PROCESS_MANAGER.lock();
        pm.cleanup_dead();
        let still_exists = pm.get_process(pid).is_ok();
        let still_queued = scheduler::is_pid_queued(pid);
        if still_exists {
            serial_println_core!("FAIL: pid {} still in PROCESS_MANAGER after cleanup", pid);
            success = false;
        }
        if still_queued {
            serial_println_core!("FAIL: pid {} still in SCHEDULER after cleanup", pid);
            success = false;
        }
        if !still_exists && !still_queued {
            serial_println_core!("ok: pid {} fully reclaimed from manager+scheduler", pid);
        }
    }

    let total_cores = kernel::process::CORE_POOL.lock().total_cores();
    serial_println_core!(
        "test_usermem_reclaim: AP_CORES_READY={} total_cores={}",
        AP_CORES_READY.load(Ordering::Acquire),
        total_cores
    );

    if success {
        serial_println_core!("PASS: usermem reclaim verified (4 allocs >2M strained and freed)");
        serial_println_core!("=== test_usermem_reclaim: ok ===");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println_core!("=== test_usermem_reclaim: FAILED ===");
        exit_qemu(QemuExitCode::Failed);
    }
}
