#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};
use kernel::filesystem::{get_vfs, init_filesystem};
use kernel::io::{disk::BlockDevice, virtio::VirtioBlk};
use kernel::process::elf_loader::{ElfLoadInfo, FS_WORKER_ELF};
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::task;
use kernel::process::{FileDescriptor, PID};
use kernel::testing::{QemuExitCode, exit_qemu, test_panic_handler};
use kernel::{boot_common, bsp_init, serial_println_core};

// indexes into the shared test counters, fs_worker uses the same values
const COUNTER_WORKER_MODE: usize = 4;
const COUNTER_HELD_FDS: usize = 6;

const MODE_HOLD: u32 = 1;

const PROGRAM_PRIORITY: u8 = 5;
const STARTUP_DEADLINE_US: u64 = 5_000_000;

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
    let device = VirtioBlk::find_and_init().expect("virtio-blk device not found");
    init_filesystem(BlockDevice::VirtioBlk(device)).expect("mounting the test disk failed");

    kernel::process::shared_state::init_shared_state();
    bsp_init::wait_for_ap_cores_blocking();
}

fn spawn_worker() -> PID {
    let elf_info = match ElfLoadInfo::from_elf_data(FS_WORKER_ELF) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("parse ELF: {:?}", error);
            fail("could not parse the fs_worker ELF");
        }
    };

    let mut process_manager = PROCESS_MANAGER.lock();
    match process_manager.create_process_from_elf(0, &elf_info, "fs_worker", PROGRAM_PRIORITY) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create the fs_worker process");
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

const KILL_EXIT_CODE: i32 = 77;
const KILL_DEADLINE_US: u64 = 2_000_000;
const HOLD_FD_COUNT: u32 = 4;
const HOLD_PATH: &str = "/HOLD.BIN";
const HOLD_WRITE_BYTES: usize = 4096;
const HELLO_PATH: &str = "/HELLO.TXT";
const AFTER_KILL_PATH: &str = "/AFTER.TXT";
const AFTER_KILL_CONTENT: &[u8] = b"still works";
const WRITES_BEFORE_KILL_US: u64 = 20_000;

fn fail(message: &str) -> ! {
    serial_println_core!("FAIL: {}", message);
    serial_println_core!("=== test_user_fs_kill: FAILED ===");
    exit_qemu(QemuExitCode::Failed);
}

fn check_filesystem_after_kill() {
    let mut vfs = get_vfs();

    // the kill only lands between syscalls, so the file holds either nothing or one whole write
    match vfs.resolve_path(HOLD_PATH) {
        Ok(node) if node.size == 0 || node.size == HOLD_WRITE_BYTES => {}
        Ok(node) => {
            serial_println_core!("hold file size {}", node.size);
            fail("the killed worker left a torn file");
        }
        Err(_) => fail("the hold file is gone"),
    }
    if vfs.delete(HOLD_PATH).is_err() {
        fail("deleting the hold file failed");
    }

    // the filesystem must still be usable
    if vfs.create_file(AFTER_KILL_PATH).is_err() {
        fail("creating a file after the kill failed");
    }
    if vfs.write_file(AFTER_KILL_PATH, 0, AFTER_KILL_CONTENT) != Ok(AFTER_KILL_CONTENT.len()) {
        fail("writing a file after the kill failed");
    }
    let mut read_back = [0u8; 16];
    let read = vfs.read_file(AFTER_KILL_PATH, 0, &mut read_back);
    if read != Ok(AFTER_KILL_CONTENT.len())
        || &read_back[..AFTER_KILL_CONTENT.len()] != AFTER_KILL_CONTENT
    {
        fail("reading back a file after the kill failed");
    }
    if vfs.delete(AFTER_KILL_PATH).is_err() {
        fail("deleting a file after the kill failed");
    }

    let mut hello = [0u8; 8];
    if vfs.read_file(HELLO_PATH, 0, &mut hello) != Ok(5) || &hello[..5] != b"hello" {
        fail("HELLO.TXT changed");
    }
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_fs_kill: start ===");
    mount_test_disk();

    counter(COUNTER_WORKER_MODE).store(MODE_HOLD, Ordering::Release);

    let pid = spawn_worker();
    wait_until("the worker to hold its fds", STARTUP_DEADLINE_US, || {
        counter(COUNTER_HELD_FDS).load(Ordering::Acquire) == HOLD_FD_COUNT
            || task::find_slot_by_pid(pid).is_none()
    });
    if task::find_slot_by_pid(pid).is_none() {
        fail("the worker exited before it was killed");
    }

    // let it rewrite the file for a while, so the kill can land mid workload
    let write_start_us = now_us();
    wait_until(
        "the worker to keep writing",
        WRITES_BEFORE_KILL_US * 100,
        || now_us() - write_start_us > WRITES_BEFORE_KILL_US,
    );

    let start_us = now_us();
    if !task::request_kill(pid, KILL_EXIT_CODE) {
        fail("request_kill found no task");
    }
    wait_until("the worker to die", KILL_DEADLINE_US, || {
        task::find_slot_by_pid(pid).is_none()
    });
    serial_println_core!(
        "test_user_fs_kill: worker gone {} us after the kill request",
        now_us() - start_us
    );

    if exit_code_of(pid) != Some(KILL_EXIT_CODE) {
        fail("killed worker has the wrong exit code");
    }
    if !fd_table_is_empty(pid) {
        fail("an fd survived the kill");
    }

    check_filesystem_after_kill();

    PROCESS_MANAGER.lock().cleanup_dead();

    serial_println_core!("=== test_user_fs_kill: ok ===");
    exit_qemu(QemuExitCode::Success);
}
