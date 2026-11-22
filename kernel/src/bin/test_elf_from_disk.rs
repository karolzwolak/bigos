#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use core::sync::atomic::Ordering;
use kernel::filesystem::vfs::{FileSystemError, VFS, get_vfs};
use kernel::process::elf_loader::{ElfLoadError, ElfLoadInfo, load_elf_from_path};
use kernel::process::process_manager::BIGPROC_PID;
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::get_shared_program_data_buffer_mut;
use kernel::process::thread;
use kernel::process::{ChildPolicy, PID, ProcessState};
use kernel::testing::{QemuExitCode, exit_qemu, mount_test_disk, test_panic_handler};
use kernel::{boot_common, bsp_init, serial_println_core};

// index into the shared test counters, busyloop uses the same value
const COUNTER_RUNNING: usize = 0;

const RUST1ST_PATH: &str = "/BIN/rust1st";
const RUST1ST_OTHER_CASE_PATH: &str = "/bin/RUST1ST";
const BUSYLOOP_PATH: &str = "/BIN/busyloop";
const MISSING_PATH: &str = "/BIN/nope";
const DIRECTORY_PATH: &str = "/BIN";
const LONG_NAME_PATH: &str = "/BIN/toolongname";
const SHORT_TEXT_PATH: &str = "/HELLO.TXT";
const NOT_ELF_PATH: &str = "/BIG.BIN";
const HEADER_ONLY_PATH: &str = "/BIN/HDRONLY.ELF";
const TRUNCATED_PATH: &str = "/BIN/TRUNC.ELF";

const BUSYLOOP_PRIORITY: u8 = 5;
const KILL_EXIT_CODE: i32 = 77;
const KILL_DEADLINE_US: u64 = 1_000_000;
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

fn fail(message: &str) -> ! {
    serial_println_core!("FAIL: {}", message);
    serial_println_core!("=== test_elf_from_disk: FAILED ===");
    exit_qemu(QemuExitCode::Failed);
}

fn expect_error(path: &str, is_expected: impl Fn(&ElfLoadError) -> bool) {
    match load_elf_from_path(path) {
        Err(error) if is_expected(&error) => {}
        other => {
            serial_println_core!("{}: unexpected result {:?}", path, other);
            fail("load_elf_from_path returned the wrong error");
        }
    }
}

fn test_from_vfs_matches_from_elf_data() {
    let from_vfs = load_elf_from_path(RUST1ST_PATH).expect("rust1st loads from disk");

    let mut vfs = get_vfs();
    let size = vfs.open_file(RUST1ST_PATH).expect("rust1st exists").size;
    let mut whole_file = vec![0u8; size];
    let read = vfs
        .read_file(RUST1ST_PATH, 0, &mut whole_file)
        .expect("whole file read");
    drop(vfs);
    if read != size {
        fail("short read of the whole rust1st file");
    }
    let from_data = ElfLoadInfo::from_elf_data(&whole_file).expect("rust1st parses from memory");

    if from_vfs.entry_point != from_data.entry_point
        || from_vfs.min_vaddr != from_data.min_vaddr
        || from_vfs.max_vaddr != from_data.max_vaddr
        || from_vfs.segments.len() != from_data.segments.len()
    {
        fail("from_vfs and from_elf_data disagree on the image layout");
    }

    for (left, right) in from_vfs.segments.iter().zip(from_data.segments.iter()) {
        if left.vaddr != right.vaddr
            || left.in_file_size != right.in_file_size
            || left.in_memory_size != right.in_memory_size
            || left.flags != right.flags
            || left.data != right.data
        {
            fail("from_vfs and from_elf_data disagree on a segment");
        }
    }
}

fn test_path_and_file_errors() {
    if load_elf_from_path(RUST1ST_OTHER_CASE_PATH).is_err() {
        fail("a path in the other case does not load");
    }

    expect_error(MISSING_PATH, |error| {
        matches!(error, ElfLoadError::FileSystem(FileSystemError::NotFound))
    });
    expect_error(DIRECTORY_PATH, |error| {
        matches!(
            error,
            ElfLoadError::FileSystem(FileSystemError::IsDirectory)
        )
    });
    expect_error(LONG_NAME_PATH, |error| {
        matches!(
            error,
            ElfLoadError::FileSystem(FileSystemError::InvalidFilename)
        )
    });
    expect_error(SHORT_TEXT_PATH, |error| {
        matches!(error, ElfLoadError::Truncated)
    });
    expect_error(NOT_ELF_PATH, |error| {
        matches!(error, ElfLoadError::InvalidMagic)
    });
    expect_error(HEADER_ONLY_PATH, |error| {
        matches!(error, ElfLoadError::Truncated)
    });
    expect_error(TRUNCATED_PATH, |error| {
        matches!(error, ElfLoadError::SegmentOutOfBounds)
    });
}

fn test_vfs_is_not_locked() {
    let _ = load_elf_from_path(RUST1ST_PATH);
    let _ = load_elf_from_path(MISSING_PATH);

    if VFS.get().expect("filesystem is mounted").is_locked() {
        fail("load_elf_from_path left the VFS locked");
    }
}

fn running_counter() -> u32 {
    // SAFETY: the page lives forever
    unsafe {
        get_shared_program_data_buffer_mut().test_counters[COUNTER_RUNNING].load(Ordering::Acquire)
    }
}

fn test_spawn_from_disk_and_kill() {
    let elf_info = load_elf_from_path(BUSYLOOP_PATH).expect("busyloop loads from disk");
    let pid: PID = match PROCESS_MANAGER.lock().create_process_from_elf(
        BIGPROC_PID,
        &elf_info,
        "busyloop",
        BUSYLOOP_PRIORITY,
    ) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create busyloop");
        }
    };

    let start_us = kernel::interrupts::tsc_timestamp_us();
    while running_counter() == 0 {
        if kernel::interrupts::tsc_timestamp_us() - start_us > STARTUP_DEADLINE_US {
            fail("busyloop from disk never started running");
        }
        core::hint::spin_loop();
    }

    if !thread::request_kill(pid, KILL_EXIT_CODE, ChildPolicy::KillChildren) {
        fail("request_kill found no thread");
    }
    let start_us = kernel::interrupts::tsc_timestamp_us();
    while thread::find_slot_by_pid(pid).is_some() {
        if kernel::interrupts::tsc_timestamp_us() - start_us > KILL_DEADLINE_US {
            fail("killed thread still alive after the deadline");
        }
        core::hint::spin_loop();
    }

    let process_manager = PROCESS_MANAGER.lock();
    match process_manager.get_process(pid) {
        Ok(process)
            if process.state == ProcessState::Terminated
                && process.exit_code == Some(KILL_EXIT_CODE) => {}
        _ => fail("killed process is not Terminated with the requested exit code"),
    }
}

fn test_main() -> ! {
    serial_println_core!("=== test_elf_from_disk: start ===");

    mount_test_disk();
    kernel::process_start::init_userspace();
    bsp_init::wait_for_ap_cores_blocking();

    test_from_vfs_matches_from_elf_data();
    test_path_and_file_errors();
    test_vfs_is_not_locked();
    test_spawn_from_disk_and_kill();

    serial_println_core!("=== test_elf_from_disk: ok ===");
    exit_qemu(QemuExitCode::Success);
}
