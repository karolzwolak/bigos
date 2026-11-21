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

const COUNTER_CHECKS_PASSED: usize = 0;
const COUNTER_CHECKS_FAILED: usize = 1;
const COUNTER_CHECKS_DONE: usize = 2;
const COUNTER_EXIT_CHILD_PID: usize = 3;
const USED_COUNTER_COUNT: usize = 4;

const LIBCTEST_PATH: &str = "/BIN/libctest";
const LIBCTEST_NAME: &str = "libctest";
const PROGRAM_PRIORITY: u8 = 5;
const DEADLINE_US: u64 = 20_000_000;
const EXPECTED_PASSED_CHECKS: u32 = 173;
const ROUND_COUNT: usize = 2;
const EXPECTED_EXIT_CHILD_CODE: i32 = 3;

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
    serial_println_core!("=== test_user_libc: FAILED ===");
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

/// libctest runs the libc shim checks, then a stdout job and an exit(3) job
fn run_round(round: usize, elf_info: &ElfLoadInfo) {
    for index in 0..USED_COUNTER_COUNT {
        counter(index).store(0, Ordering::Release);
    }

    let libctest_pid = match PROCESS_MANAGER.lock().create_process_from_elf(
        0,
        elf_info,
        LIBCTEST_NAME,
        PROGRAM_PRIORITY,
    ) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create libctest");
        }
    };

    wait_until("libctest to exit", || is_gone(libctest_pid));
    if counter(COUNTER_CHECKS_DONE).load(Ordering::Acquire) != 1 {
        fail("libctest exited before finishing its checks");
    }

    let passed = counter(COUNTER_CHECKS_PASSED).load(Ordering::Acquire);
    let failed = counter(COUNTER_CHECKS_FAILED).load(Ordering::Acquire);
    serial_println_core!(
        "test_user_libc: round {}: {} checks passed, {} failed",
        round,
        passed,
        failed
    );
    if failed != 0 {
        fail("libctest counted failed checks");
    }
    if passed != EXPECTED_PASSED_CHECKS {
        fail("libctest ran an unexpected number of checks");
    }

    check_clean_exit(libctest_pid, LIBCTEST_NAME);

    let exit_child_pid = counter(COUNTER_EXIT_CHILD_PID).load(Ordering::Acquire) as PID;
    let exit_child_code = PROCESS_MANAGER
        .lock()
        .get_process(exit_child_pid)
        .ok()
        .map(|process| (process.state, process.exit_code));
    if exit_child_code != Some((ProcessState::Terminated, Some(EXPECTED_EXIT_CHILD_CODE))) {
        serial_println_core!("exit child (pid {}): {:?}", exit_child_pid, exit_child_code);
        fail("exit(3) did not end the child with code 3");
    }

    let live_channels = live_channel_count();
    if live_channels != 0 {
        serial_println_core!("{} channels still live", live_channels);
        fail("channels leaked");
    }

    PROCESS_MANAGER.lock().cleanup_dead();
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_libc: start ===");

    mount_test_disk();
    kernel::process_start::init_userspace();
    bsp_init::wait_for_ap_cores_blocking();

    let elf_info = match load_elf_from_path(LIBCTEST_PATH) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("load libctest: {:?}", error);
            fail("could not load the libctest ELF");
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
        "test_user_libc: free frames {} before, {} after",
        start_free_frames,
        end_free_frames
    );
    if end_free_frames != start_free_frames {
        fail("free frame count changed over a round");
    }

    serial_println_core!("=== test_user_libc: ok ===");
    exit_qemu(QemuExitCode::Success);
}
