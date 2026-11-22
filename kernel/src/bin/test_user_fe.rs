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

const FETEST_PATH: &str = "/BIN/fetest";
const FETEST_NAME: &str = "fetest";
const FETEST_DIR: &str = "/FET";
const PROGRAM_PRIORITY: u8 = 5;
const DEADLINE_US: u64 = 25_000_000;
const MIN_PASSED_CHECKS: u32 = 141;

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
    serial_println_core!("=== test_user_fe: FAILED ===");
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

fn spawn_fetest() -> PID {
    let elf_info = match load_elf_from_path(FETEST_PATH) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("load fetest: {:?}", error);
            fail("could not load the fetest ELF");
        }
    };

    match PROCESS_MANAGER.lock().create_process_from_elf(
        BIGPROC_PID,
        &elf_info,
        FETEST_NAME,
        PROGRAM_PRIORITY,
    ) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create fetest");
        }
    }
}

fn check_fetest_exit(pid: PID) {
    let process_manager = PROCESS_MANAGER.lock();
    let Ok(process) = process_manager.get_process(pid) else {
        fail("fetest is gone before its exit was checked");
    };

    if process.state != ProcessState::Terminated || process.exit_code != Some(0) {
        serial_println_core!(
            "fetest state {:?} exit code {:?}",
            process.state,
            process.exit_code
        );
        fail("fetest did not exit cleanly with 0");
    }
}

fn check_counters() {
    let passed = counter(COUNTER_CHECKS_PASSED).load(Ordering::Acquire);
    let failed = counter(COUNTER_CHECKS_FAILED).load(Ordering::Acquire);
    serial_println_core!("test_user_fe: {} checks passed, {} failed", passed, failed);

    if counter(COUNTER_CHECKS_DONE).load(Ordering::Acquire) != 1 {
        fail("fetest exited before finishing its checks");
    }
    if failed != 0 {
        fail("fetest counted failed checks");
    }
    if passed < MIN_PASSED_CHECKS {
        fail("fetest ran fewer checks than expected");
    }
}

fn check_test_directory_removed() {
    match get_vfs().resolve_path(FETEST_DIR) {
        Err(FileSystemError::NotFound) => {}
        Ok(_) => fail("fetest left its directory behind"),
        Err(error) => {
            serial_println_core!("resolve {}: {:?}", FETEST_DIR, error);
            fail("unexpected error while looking for the fetest directory");
        }
    }
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_fe: start ===");

    mount_test_disk();
    kernel::process_start::init_userspace();
    bsp_init::wait_for_ap_cores_blocking();

    let pid = spawn_fetest();
    wait_until("fetest to exit", || thread::find_slot_by_pid(pid).is_none());

    check_fetest_exit(pid);
    check_counters();
    check_test_directory_removed();

    serial_println_core!("=== test_user_fe: ok ===");
    exit_qemu(QemuExitCode::Success);
}
