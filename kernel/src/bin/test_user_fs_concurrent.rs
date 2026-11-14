#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};
use kernel::filesystem::{get_vfs, vfs::FileSystemError};
use kernel::process::elf_loader::load_elf_from_path;
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::task;
use kernel::process::{FileDescriptor, PID};
use kernel::testing::{QemuExitCode, exit_qemu, test_panic_handler};
use kernel::{boot_common, bsp_init, serial_println_core};

// indexes into the shared test counters, fsworker uses the same values
const COUNTER_CHECKS_PASSED: usize = 1;
const COUNTER_CHECKS_FAILED: usize = 2;
const COUNTER_WORKER_MODE: usize = 4;
const COUNTER_WORKERS_DONE: usize = 5;

const MODE_WORK: u32 = 0;

const PROGRAM_PRIORITY: u8 = 5;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    unsafe { boot_common::bsp_early_init() };
    test_main();
}

fn counter(index: usize) -> &'static AtomicU32 {
    debug_assert!(index < TEST_COUNTER_COUNT);

    // SAFETY: the shared page lives forever and the counters are atomics
    unsafe { &get_shared_program_data_buffer_mut().test_counters[index] }
}

fn now_us() -> u64 {
    kernel::interrupts::tsc_timestamp_us()
}

fn mount_test_disk() {
    kernel::testing::mount_test_disk();

    kernel::process_start::init_userspace();
    bsp_init::wait_for_ap_cores_blocking();
}

fn spawn_worker() -> PID {
    let elf_info = match load_elf_from_path("/BIN/fsworker") {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("parse ELF: {:?}", error);
            fail("could not parse the fsworker ELF");
        }
    };

    let mut process_manager = PROCESS_MANAGER.lock();
    match process_manager.create_process_from_elf(0, &elf_info, "fsworker", PROGRAM_PRIORITY) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create the fsworker process");
        }
    }
}

fn wait_until(what: &str, deadline_us: u64, condition: impl Fn() -> bool) {
    let start_us = now_us();

    while !condition() {
        if now_us() - start_us > deadline_us {
            serial_println_core!("timed out waiting for: {}", what);
            fail("timeout");
        }
        core::hint::spin_loop();
    }
}

fn exit_code_of(pid: PID) -> Option<i32> {
    let process_manager = PROCESS_MANAGER.lock();
    let process = process_manager.get_process(pid).ok()?;
    if process.state != kernel::process::ProcessState::Terminated {
        return None;
    }

    process.exit_code
}

fn fd_table_is_empty(pid: PID) -> bool {
    let process_manager = PROCESS_MANAGER.lock();
    let Ok(process) = process_manager.get_process(pid) else {
        return false;
    };

    process
        .file_descriptors
        .iter()
        .all(|descriptor| *descriptor == FileDescriptor::CLOSED)
}

const WORKER_COUNT: usize = 2;
const DEADLINE_US: u64 = 60_000_000;
const MIN_PASSED_CHECKS: u32 = 200;

fn fail(message: &str) -> ! {
    serial_println_core!("FAIL: {}", message);
    serial_println_core!("=== test_user_fs_concurrent: FAILED ===");
    exit_qemu(QemuExitCode::Failed);
}

fn home_core_of(pid: PID) -> u8 {
    let slot_index = task::find_slot_by_pid(pid).expect("a new task has a slot");
    task::slot(slot_index).home_core.load(Ordering::Relaxed)
}

fn check_worker_directories_are_gone(pids: &[PID; WORKER_COUNT]) {
    let mut vfs = get_vfs();

    for pid in pids {
        let path = alloc::format!("/W{}", pid);
        match vfs.resolve_path(&path) {
            Err(FileSystemError::NotFound) => {}
            _ => fail("a worker directory is still there"),
        }
    }
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_fs_concurrent: start ===");
    mount_test_disk();

    counter(COUNTER_WORKER_MODE).store(MODE_WORK, Ordering::Release);

    let pids = [spawn_worker(), spawn_worker()];
    if home_core_of(pids[0]) == home_core_of(pids[1]) {
        fail("both workers share a core, so the filesystem is never used in parallel");
    }

    let start_us = now_us();
    for pid in pids {
        wait_until("a worker to exit", DEADLINE_US, || {
            task::find_slot_by_pid(pid).is_none()
        });
    }
    serial_println_core!(
        "test_user_fs_concurrent: workers took {} us",
        now_us() - start_us
    );

    for pid in pids {
        if exit_code_of(pid) != Some(0) {
            serial_println_core!("worker {} exit code {:?}", pid, exit_code_of(pid));
            fail("a worker reported a failed check");
        }
        if !fd_table_is_empty(pid) {
            fail("an fd survived the worker teardown");
        }
    }

    let passed = counter(COUNTER_CHECKS_PASSED).load(Ordering::Acquire);
    let failed = counter(COUNTER_CHECKS_FAILED).load(Ordering::Acquire);
    serial_println_core!(
        "test_user_fs_concurrent: {} checks passed, {} failed",
        passed,
        failed
    );
    if failed != 0 {
        fail("workers counted failed checks");
    }
    if passed < MIN_PASSED_CHECKS {
        fail("workers ran fewer checks than expected");
    }
    if counter(COUNTER_WORKERS_DONE).load(Ordering::Acquire) != WORKER_COUNT as u32 {
        fail("not every worker finished its rounds");
    }

    check_worker_directories_are_gone(&pids);

    PROCESS_MANAGER.lock().cleanup_dead();

    serial_println_core!("=== test_user_fs_concurrent: ok ===");
    exit_qemu(QemuExitCode::Success);
}
