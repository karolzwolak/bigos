#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};
use kernel::filesystem::{get_vfs, vfs::FileSystemError};
use kernel::process::elf_loader::load_elf_from_path;
use kernel::process::process_manager::BIGPROC_PID;
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::thread;
use kernel::process::{PID, ProcessState};
use kernel::testing::{QemuExitCode, exit_qemu, mount_test_disk, test_panic_handler};
use kernel::{boot_common, bsp_init, serial_println_core};

const COUNTER_CHECKS_PASSED: usize = 0;
const COUNTER_CHECKS_FAILED: usize = 1;
const COUNTER_CHECKS_DONE: usize = 2;
const COUNTER_EXITED_CHILD_PID: usize = 3;
const COUNTER_KILLED_CHILD_PID: usize = 4;

const SHTEST_PATH: &str = "/BIN/shtest";
const SHTEST_NAME: &str = "shtest";
const SHTEST_DIR: &str = "/SHT";
const PROGRAM_PRIORITY: u8 = 5;
const DEADLINE_US: u64 = 25_000_000;
const MIN_PASSED_CHECKS: u32 = 180;

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
    serial_println_core!("=== test_user_shell: FAILED ===");
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

fn spawn_shtest() -> PID {
    let elf_info = match load_elf_from_path(SHTEST_PATH) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("load shtest: {:?}", error);
            fail("could not load the shtest ELF");
        }
    };

    match PROCESS_MANAGER.lock().create_process_from_elf(
        BIGPROC_PID,
        &elf_info,
        SHTEST_NAME,
        PROGRAM_PRIORITY,
    ) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create shtest");
        }
    }
}

fn check_shtest_exit(pid: PID) {
    let process_manager = PROCESS_MANAGER.lock();
    let Ok(process) = process_manager.get_process(pid) else {
        fail("shtest is gone before its exit was checked");
    };

    if process.state != ProcessState::Terminated || process.exit_code != Some(0) {
        serial_println_core!(
            "shtest state {:?} exit code {:?}",
            process.state,
            process.exit_code
        );
        fail("shtest did not exit cleanly with 0");
    }
}

/// Both children were waited for, so WaitProcess has to have removed their entries
fn check_children_removed() {
    let exited_pid = counter(COUNTER_EXITED_CHILD_PID).load(Ordering::Acquire) as PID;
    let killed_pid = counter(COUNTER_KILLED_CHILD_PID).load(Ordering::Acquire) as PID;
    if exited_pid == 0 || killed_pid == 0 {
        fail("shtest did not spawn both children");
    }

    let process_manager = PROCESS_MANAGER.lock();
    for pid in [exited_pid, killed_pid] {
        if process_manager.get_process(pid).is_ok() {
            serial_println_core!("pid {} is still listed", pid);
            fail("a waited for child was not removed");
        }
    }
    drop(process_manager);

    for pid in [exited_pid, killed_pid] {
        if thread::find_slot_by_pid(pid).is_some() {
            serial_println_core!("pid {} still has a thread", pid);
            fail("a removed child kept its thread");
        }
    }
}

fn check_counters() {
    let passed = counter(COUNTER_CHECKS_PASSED).load(Ordering::Acquire);
    let failed = counter(COUNTER_CHECKS_FAILED).load(Ordering::Acquire);
    serial_println_core!(
        "test_user_shell: {} checks passed, {} failed",
        passed,
        failed
    );

    if counter(COUNTER_CHECKS_DONE).load(Ordering::Acquire) != 1 {
        fail("shtest exited before finishing its checks");
    }
    if failed != 0 {
        fail("shtest counted failed checks");
    }
    if passed < MIN_PASSED_CHECKS {
        fail("shtest ran fewer checks than expected");
    }
}

fn check_test_directory_removed() {
    match get_vfs().resolve_path(SHTEST_DIR) {
        Err(FileSystemError::NotFound) => {}
        Ok(_) => fail("rm -r left the shtest directory behind"),
        Err(error) => {
            serial_println_core!("resolve {}: {:?}", SHTEST_DIR, error);
            fail("unexpected error while looking for the shtest directory");
        }
    }
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_shell: start ===");

    mount_test_disk();
    kernel::process_start::init_userspace();
    bsp_init::wait_for_ap_cores_blocking();

    let pid = spawn_shtest();
    wait_until("shtest to exit", || thread::find_slot_by_pid(pid).is_none());

    check_shtest_exit(pid);
    check_counters();
    check_children_removed();
    check_test_directory_removed();

    serial_println_core!("=== test_user_shell: ok ===");
    exit_qemu(QemuExitCode::Success);
}
