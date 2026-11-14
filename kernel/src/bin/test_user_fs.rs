#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use core::sync::atomic::{AtomicU32, Ordering};
use kernel::filesystem::{get_vfs, init_filesystem, vfs::FileSystemError};
use kernel::io::{disk::BlockDevice, virtio::VirtioBlk};
use kernel::process::elf_loader::{ElfLoadInfo, FS_TEST_ELF};
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::task;
use kernel::process::{FileDescriptor, PID, ProcessState};
use kernel::testing::{QemuExitCode, exit_qemu, test_panic_handler};
use kernel::{boot_common, bsp_init, serial_println_core};

// indexes into the shared test counters, fs_test uses the same values
const COUNTER_RUNNING: usize = 0;
const COUNTER_CHECKS_PASSED: usize = 1;
const COUNTER_CHECKS_FAILED: usize = 2;
const COUNTER_LEAKED_FDS: usize = 3;

const PROGRAM_PRIORITY: u8 = 5;
const STARTUP_DEADLINE_US: u64 = 5_000_000;
const DEADLINE_US: u64 = 20_000_000;
const EXPECTED_LEAKED_FDS: u32 = 3;
// fs_test runs well over this many checks, a lower count means it stopped early
const MIN_PASSED_CHECKS: u32 = 150;

// files fs_test creates and deletes again
const DELETED_PATHS: [&str; 7] = [
    "/FSTEST.TXT",
    "/FSSEQ.BIN",
    "/FSBIG.BIN",
    "/FSOVER.BIN",
    "/FSERR.TXT",
    "/FSDIR",
    "/FSPAGE",
];
const HELLO_PATH: &str = "/HELLO.TXT";
const HELLO_CONTENT: &[u8] = b"hello";
const BIG_PATH: &str = "/BIG.BIN";
const BIG_FILE_SIZE_BYTES: usize = 100_000;

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
    serial_println_core!("=== test_user_fs: FAILED ===");
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

fn spawn_fs_test() -> PID {
    let elf_info = match ElfLoadInfo::from_elf_data(FS_TEST_ELF) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("parse ELF: {:?}", error);
            fail("could not parse the fs_test ELF");
        }
    };

    let mut process_manager = PROCESS_MANAGER.lock();
    match process_manager.create_process_from_elf(0, &elf_info, "fs_test", PROGRAM_PRIORITY) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create the fs_test process");
        }
    }
}

fn check_exit(pid: PID) {
    let process_manager = PROCESS_MANAGER.lock();
    let Ok(process) = process_manager.get_process(pid) else {
        fail("fs_test process is gone");
    };

    if process.state != ProcessState::Terminated {
        fail("fs_test is not terminated");
    }
    if process.exit_code != Some(0) {
        serial_println_core!("fs_test exit code {:?}", process.exit_code);
        fail("fs_test reported a failed check");
    }

    // fs_test exits with open fds, teardown has to release every one of them
    if process
        .file_descriptors
        .iter()
        .any(|descriptor| *descriptor != FileDescriptor::CLOSED)
    {
        fail("an fd survived the process teardown");
    }
}

fn check_counters() {
    let passed = counter(COUNTER_CHECKS_PASSED).load(Ordering::Acquire);
    let failed = counter(COUNTER_CHECKS_FAILED).load(Ordering::Acquire);
    let leaked = counter(COUNTER_LEAKED_FDS).load(Ordering::Acquire);
    serial_println_core!(
        "test_user_fs: {} checks passed, {} failed, {} fds left open",
        passed,
        failed,
        leaked
    );

    if failed != 0 {
        fail("fs_test counted failed checks");
    }
    if passed < MIN_PASSED_CHECKS {
        fail("fs_test ran fewer checks than expected");
    }
    if leaked != EXPECTED_LEAKED_FDS {
        fail("fs_test did not leave its fds open");
    }
}

fn check_filesystem_state() {
    let mut vfs = get_vfs();

    for path in DELETED_PATHS {
        match vfs.resolve_path(path) {
            Err(FileSystemError::NotFound) => {}
            Ok(_) => {
                serial_println_core!("{} still exists", path);
                fail("a file fs_test deleted is still there");
            }
            Err(error) => {
                serial_println_core!("resolve {}: {:?}", path, error);
                fail("unexpected error while looking for a deleted file");
            }
        }
    }

    let mut hello = vec![0u8; HELLO_CONTENT.len()];
    if vfs.read_file(HELLO_PATH, 0, &mut hello) != Ok(HELLO_CONTENT.len()) || hello != HELLO_CONTENT
    {
        fail("HELLO.TXT changed");
    }

    match vfs.resolve_path(BIG_PATH) {
        Ok(node) if node.size == BIG_FILE_SIZE_BYTES => {}
        _ => fail("BIG.BIN changed"),
    }
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_fs: start ===");

    let device = VirtioBlk::find_and_init().expect("virtio-blk device not found");
    init_filesystem(BlockDevice::VirtioBlk(device)).expect("mounting the test disk failed");

    kernel::process::shared_state::init_shared_state();
    bsp_init::wait_for_ap_cores_blocking();

    let pid = spawn_fs_test();
    wait_until("fs_test to start", STARTUP_DEADLINE_US, || {
        counter(COUNTER_RUNNING).load(Ordering::Acquire) == 1
            || task::find_slot_by_pid(pid).is_none()
    });

    let start_us = now_us();
    wait_until("fs_test to exit", DEADLINE_US, || {
        task::find_slot_by_pid(pid).is_none()
    });
    serial_println_core!("test_user_fs: fs_test took {} us", now_us() - start_us);

    check_exit(pid);
    check_counters();
    check_filesystem_state();

    PROCESS_MANAGER.lock().cleanup_dead();

    serial_println_core!("=== test_user_fs: ok ===");
    exit_qemu(QemuExitCode::Success);
}
