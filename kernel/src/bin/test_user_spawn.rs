#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};
use kernel::process::elf_loader::load_elf_from_path;
use kernel::process::process_manager::{BIGPROC_PID, PROCESS_MANAGER};
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::task;
use kernel::process::{FileDescriptor, PID, Process, ProcessState};
use kernel::testing::{QemuExitCode, exit_qemu, mount_test_disk, test_panic_handler};
use kernel::{boot_common, bsp_init, serial_println_core};

// indexes into the shared test counters, spawner, spawnee and treeleaf use the same values
// fs_test uses 0..=3 and fsworker 4..=6, which never run in the same test kernel
const COUNTER_LEAVES_STARTED: usize = 3;
const COUNTER_LEAF_RELEASE: usize = 4;
const COUNTER_DETACHED_PID: usize = 7;
const COUNTER_RUNNING: usize = 8;
const COUNTER_CHECKS_PASSED: usize = 9;
const COUNTER_CHECKS_FAILED: usize = 10;
const COUNTER_CHILD_PID: usize = 11;
const COUNTER_SPAWNEE_PID: usize = 12;
const COUNTER_RELEASE: usize = 13;
const COUNTER_CHECKS_DONE: usize = 14;
const COUNTER_SPAWNEE_ARGS: usize = 15;
// value of COUNTER_SPAWNEE_ARGS when spawnee saw the expected args and read the inherited fd
const SPAWNEE_ARGS_OK: u32 = 1;

const SPAWNER_PATH: &str = "/BIN/spawner";
const SPAWNER_NAME: &str = "spawner";
const SPAWNEE_NAME: &str = "kid";
const DETACHED_NAME: &str = "treeleaf";
const PROGRAM_PRIORITY: u8 = 5;
const STARTUP_DEADLINE_US: u64 = 5_000_000;
const DEADLINE_US: u64 = 20_000_000;
const EXPECTED_PASSED_CHECKS: u32 = 19;

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
    serial_println_core!("=== test_user_spawn: FAILED ===");
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

fn spawn_spawner() -> PID {
    let elf_info = match load_elf_from_path(SPAWNER_PATH) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("load spawner: {:?}", error);
            fail("could not load the spawner ELF");
        }
    };

    let mut process_manager = PROCESS_MANAGER.lock();
    match process_manager.create_process_from_elf(0, &elf_info, SPAWNER_NAME, PROGRAM_PRIORITY) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create the spawner process");
        }
    }
}

fn check_clean_exit(process: &Process, what: &str) {
    if process.state != ProcessState::Terminated {
        serial_println_core!("{} state {:?}", what, process.state);
        fail("a process is not terminated");
    }
    if process.exit_code != Some(0) {
        serial_println_core!("{} exit code {:?}", what, process.exit_code);
        fail("a process did not exit with 0");
    }
    if process
        .file_descriptors
        .iter()
        .any(|descriptor| *descriptor != FileDescriptor::CLOSED)
    {
        fail("an fd leaked");
    }
}

/// DETACHED: the child hangs off bigproc, not off the spawner
fn check_detached_child(spawner_pid: PID) -> PID {
    let detached_pid = counter(COUNTER_DETACHED_PID).load(Ordering::Acquire) as PID;
    if detached_pid == 0 {
        fail("spawner did not report the detached child");
    }
    wait_until("the detached child to start", STARTUP_DEADLINE_US, || {
        counter(COUNTER_LEAVES_STARTED).load(Ordering::Acquire) == 1
    });

    let process_manager = PROCESS_MANAGER.lock();
    let Ok(detached) = process_manager.get_process(detached_pid) else {
        fail("the detached child does not exist");
    };
    if detached.parent_pid != BIGPROC_PID {
        serial_println_core!("detached child parent {}", detached.parent_pid);
        fail("the detached child is not parented to bigproc");
    }
    match (
        process_manager.get_process(BIGPROC_PID),
        process_manager.get_process(spawner_pid),
    ) {
        (Ok(bigproc), Ok(spawner))
            if bigproc.children.contains(&detached_pid)
                && !spawner.children.contains(&detached_pid) => {}
        _ => fail("the detached child is listed under the wrong parent"),
    }

    detached_pid
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_spawn: start ===");

    mount_test_disk();
    kernel::process_start::init_userspace();
    bsp_init::wait_for_ap_cores_blocking();

    let spawner_pid = spawn_spawner();
    wait_until("spawner to start", STARTUP_DEADLINE_US, || {
        counter(COUNTER_RUNNING).load(Ordering::Acquire) == 1
            || task::find_slot_by_pid(spawner_pid).is_none()
    });
    wait_until("spawner checks", DEADLINE_US, || {
        counter(COUNTER_CHECKS_DONE).load(Ordering::Acquire) == 1
            || task::find_slot_by_pid(spawner_pid).is_none()
    });

    let child_pid = counter(COUNTER_CHILD_PID).load(Ordering::Acquire) as PID;
    if child_pid == 0 || child_pid == spawner_pid {
        fail("spawner did not report a distinct child pid");
    }
    wait_until("spawnee to start", STARTUP_DEADLINE_US, || {
        counter(COUNTER_SPAWNEE_PID).load(Ordering::Acquire) as PID == child_pid
    });

    {
        let process_manager = PROCESS_MANAGER.lock();
        let Ok(child) = process_manager.get_process(child_pid) else {
            fail("the child process does not exist");
        };
        if child.parent_pid != spawner_pid {
            serial_println_core!("child parent {} spawner {}", child.parent_pid, spawner_pid);
            fail("the child is not parented to the spawner");
        }
        if child.name != SPAWNEE_NAME {
            serial_println_core!("child name {}", child.name);
            fail("the child is not named after its file");
        }
        if child.priority != PROGRAM_PRIORITY {
            fail("the child did not get the requested priority");
        }

        let Ok(spawner) = process_manager.get_process(spawner_pid) else {
            fail("the spawner process does not exist");
        };
        if !spawner.children.contains(&child_pid) {
            fail("the spawner does not list the child");
        }

        // INHERIT_FDS: spawner keeps its fds open until it is released
        if child.file_descriptors != spawner.file_descriptors {
            fail("the child fd table is not a copy of the spawner's");
        }
        if !child
            .file_descriptors
            .iter()
            .any(|descriptor| descriptor.is_open())
        {
            fail("the child inherited no open fd");
        }
    }

    if counter(COUNTER_SPAWNEE_ARGS).load(Ordering::Acquire) != SPAWNEE_ARGS_OK {
        fail("spawnee did not get its args or could not read the inherited fd");
    }

    let detached_pid = check_detached_child(spawner_pid);

    counter(COUNTER_RELEASE).store(1, Ordering::Release);
    counter(COUNTER_LEAF_RELEASE).store(1, Ordering::Release);
    wait_until("every process to exit", DEADLINE_US, || {
        [spawner_pid, child_pid, detached_pid]
            .iter()
            .all(|&pid| task::find_slot_by_pid(pid).is_none())
    });

    let passed = counter(COUNTER_CHECKS_PASSED).load(Ordering::Acquire);
    let failed = counter(COUNTER_CHECKS_FAILED).load(Ordering::Acquire);
    serial_println_core!(
        "test_user_spawn: {} checks passed, {} failed",
        passed,
        failed
    );
    if failed != 0 {
        fail("spawner counted failed checks");
    }
    if passed != EXPECTED_PASSED_CHECKS {
        fail("spawner ran an unexpected number of checks");
    }

    {
        let process_manager = PROCESS_MANAGER.lock();
        for (pid, what) in [
            (spawner_pid, SPAWNER_NAME),
            (child_pid, SPAWNEE_NAME),
            (detached_pid, DETACHED_NAME),
        ] {
            match process_manager.get_process(pid) {
                Ok(process) => check_clean_exit(process, what),
                Err(_) => fail("a process is gone before its exit was checked"),
            }
        }
    }
    PROCESS_MANAGER.lock().cleanup_dead();

    serial_println_core!("=== test_user_spawn: ok ===");
    exit_qemu(QemuExitCode::Success);
}
