#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};
use kernel::ipc::channel::live_channel_count;
use kernel::process::elf_loader::load_elf_from_path;
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::task;
use kernel::process::{ElfLoadInfo, PID, ProcessState};
use kernel::testing::{QemuExitCode, exit_qemu, mount_test_disk, test_panic_handler};
use kernel::{boot_common, bsp_init, serial_println_core};

// indexes into the shared test counters, runjob uses the same values
const COUNTER_CHECKS_PASSED: usize = 0;
const COUNTER_CHECKS_FAILED: usize = 1;
const COUNTER_CHECKS_DONE: usize = 2;
const COUNTER_LSPROC_PID: usize = 3;
const USED_COUNTER_COUNT: usize = 4;

const RUNJOB_PATH: &str = "/BIN/runjob";
const RUNJOB_NAME: &str = "runjob";
const PROGRAM_PRIORITY: u8 = 5;
const DEADLINE_US: u64 = 20_000_000;
const EXPECTED_PASSED_CHECKS: u32 = 23;
const ROUND_COUNT: usize = 2;
// lsproc prints only through the direct ring path
const LSPROC_WRITE_SYSCALLS: u32 = 0;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    unsafe { boot_common::bsp_early_init() };
    test_main();
}

fn fail(message: &str) -> ! {
    serial_println_core!("FAIL: {}", message);
    serial_println_core!("=== test_user_run: FAILED ===");
    exit_qemu(QemuExitCode::Failed);
}

fn counter(index: usize) -> &'static AtomicU32 {
    debug_assert!(index < TEST_COUNTER_COUNT);

    // SAFETY: the shared page lives forever and the counters are atomics
    unsafe { &get_shared_program_data_buffer_mut().test_counters[index] }
}

fn now_us() -> u64 {
    kernel::interrupts::tsc_timestamp_us()
}

fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let start_us = now_us();

    while !condition() {
        if now_us() - start_us > DEADLINE_US {
            serial_println_core!("timed out waiting for: {}", what);
            fail("timeout");
        }
        core::hint::spin_loop();
    }
}

fn is_gone(pid: PID) -> bool {
    task::find_slot_by_pid(pid).is_none()
}

fn check_clean_exit(pid: PID, what: &str) {
    let process_manager = PROCESS_MANAGER.lock();
    let Ok(process) = process_manager.get_process(pid) else {
        serial_println_core!("{} (pid {}) is missing", what, pid);
        fail("a process is gone before its exit was checked");
    };

    if process.state != ProcessState::Terminated || process.exit_code != Some(0) {
        serial_println_core!(
            "{} state {:?} exit code {:?}",
            what,
            process.state,
            process.exit_code
        );
        fail("a process did not exit cleanly with 0");
    }
}

/// runjob drives ShellJob, the code behind the shell's run command, through lsproc and outwrite
fn run_round(round: usize, elf_info: &ElfLoadInfo) {
    for index in 0..USED_COUNTER_COUNT {
        counter(index).store(0, Ordering::Release);
    }

    let runjob_pid = match PROCESS_MANAGER.lock().create_process_from_elf(
        0,
        elf_info,
        RUNJOB_NAME,
        PROGRAM_PRIORITY,
    ) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create runjob");
        }
    };

    wait_until("runjob to exit", || is_gone(runjob_pid));
    if counter(COUNTER_CHECKS_DONE).load(Ordering::Acquire) != 1 {
        fail("runjob exited before finishing its checks");
    }

    let passed = counter(COUNTER_CHECKS_PASSED).load(Ordering::Acquire);
    let failed = counter(COUNTER_CHECKS_FAILED).load(Ordering::Acquire);
    serial_println_core!(
        "test_user_run: round {}: {} checks passed, {} failed",
        round,
        passed,
        failed
    );
    if failed != 0 {
        fail("runjob counted failed checks");
    }
    if passed != EXPECTED_PASSED_CHECKS {
        fail("runjob ran an unexpected number of checks");
    }

    check_clean_exit(runjob_pid, RUNJOB_NAME);

    let lsproc_pid = counter(COUNTER_LSPROC_PID).load(Ordering::Acquire) as PID;
    check_clean_exit(lsproc_pid, "lsproc");
    let lsproc_write_syscalls = PROCESS_MANAGER
        .lock()
        .get_process(lsproc_pid)
        .map(|process| process.stdout_write_syscall_count)
        .unwrap_or(u32::MAX);
    if lsproc_write_syscalls != LSPROC_WRITE_SYSCALLS {
        serial_println_core!(
            "lsproc made {} stdout Write syscalls",
            lsproc_write_syscalls
        );
        fail("lsproc printed through the syscall");
    }

    let live_channels = live_channel_count();
    if live_channels != 0 {
        serial_println_core!("{} channels still live", live_channels);
        fail("channels leaked");
    }

    PROCESS_MANAGER.lock().cleanup_dead();
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_run: start ===");

    mount_test_disk();
    kernel::process_start::init_userspace();
    bsp_init::wait_for_ap_cores_blocking();

    let elf_info = match load_elf_from_path(RUNJOB_PATH) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("load runjob: {:?}", error);
            fail("could not load the runjob ELF");
        }
    };

    // the first round settles the frame allocator, frames may come from the bump region
    // and only later end up on the free list
    run_round(0, &elf_info);
    let start_free_frames = kernel::memory::get_frame_allocator().free_frame_count();

    for round in 1..ROUND_COUNT {
        run_round(round, &elf_info);
    }

    let end_free_frames = kernel::memory::get_frame_allocator().free_frame_count();
    serial_println_core!(
        "test_user_run: free frames {} before, {} after",
        start_free_frames,
        end_free_frames
    );
    if end_free_frames != start_free_frames {
        fail("free frame count changed over a round");
    }

    serial_println_core!("=== test_user_run: ok ===");
    exit_qemu(QemuExitCode::Success);
}
