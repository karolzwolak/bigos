#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};
use kernel::ipc::channel::live_channel_count;
use kernel::process::elf_loader::load_elf_from_path;
use kernel::process::process_manager::BIGPROC_PID;
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::thread;
use kernel::process::{ElfLoadInfo, PID, ProcessState};
use kernel::testing::{QemuExitCode, exit_qemu, mount_test_disk, test_panic_handler};
use kernel::{boot_common, bsp_init, serial_println_core};

// indexes into the shared test counters, outread and outwrite use the same values
const COUNTER_CHECKS_PASSED: usize = 0;
const COUNTER_CHECKS_FAILED: usize = 1;
const COUNTER_CHECKS_DONE: usize = 2;
const COUNTER_BASIC_PID: usize = 3;
const COUNTER_FLOOD_PID: usize = 4;
const COUNTER_ORPHAN_PID: usize = 5;
const COUNTER_READER_EXITING: usize = 6;
const COUNTER_ORPHAN_WRITES_DONE: usize = 7;
const COUNTER_FLOOD_SYSCALL_PID: usize = 8;
const USED_COUNTER_COUNT: usize = 9;

const OUTREAD_PATH: &str = "/BIN/outread";
const OUTREAD_NAME: &str = "outread";
const PROGRAM_PRIORITY: u8 = 5;
const DEADLINE_US: u64 = 20_000_000;
const EXPECTED_PASSED_CHECKS: u32 = 30;

// Write syscalls on fd 1 and 2 each outwrite mode makes, prints go straight into the ring
// basic: only the stderr line goes through the syscall
const BASIC_WRITE_SYSCALLS: u32 = 1;
// flood and orphan: every byte goes straight into the ring
const DIRECT_WRITE_SYSCALLS: u32 = 0;
// floodsys: outwrite FLOOD_TOTAL_BYTES in FLOOD_CHUNK_BYTES chunks, one syscall each
const FLOOD_TOTAL_BYTES: usize = 48 * 1024;
const FLOOD_CHUNK_BYTES: usize = 1000;
const FLOOD_SYSCALL_WRITE_SYSCALLS: u32 = FLOOD_TOTAL_BYTES.div_ceil(FLOOD_CHUNK_BYTES) as u32;
const ROUND_COUNT: usize = 2;

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
    serial_println_core!("=== test_user_stdout: FAILED ===");
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
    thread::find_slot_by_pid(pid).is_none()
}

fn check_clean_exit(pid: PID, what: &str, expected_write_syscalls: u32) {
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
    if process.channels.iter().any(|mapping| mapping.is_mapped()) {
        fail("a terminated process still lists a channel");
    }
    if process.stdout_write_syscall_count != expected_write_syscalls {
        serial_println_core!(
            "{} made {} stdout Write syscalls, expected {}",
            what,
            process.stdout_write_syscall_count,
            expected_write_syscalls
        );
        fail("stdout Write syscall count is off, prints still trap or writes went missing");
    }
}

fn reported_pid(index: usize, what: &str) -> PID {
    let pid = counter(index).load(Ordering::Acquire) as PID;
    if pid == 0 {
        serial_println_core!("no pid for {}", what);
        fail("outread did not report a writer pid");
    }

    pid
}

/// outread runs every check and leaves an orphan writer behind, which must finish on its own
fn run_round(round: usize, elf_info: &ElfLoadInfo) {
    for index in 0..USED_COUNTER_COUNT {
        counter(index).store(0, Ordering::Release);
    }

    let outread_pid = match PROCESS_MANAGER.lock().create_process_from_elf(
        BIGPROC_PID,
        elf_info,
        OUTREAD_NAME,
        PROGRAM_PRIORITY,
    ) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create outread");
        }
    };

    wait_until("outread checks", || {
        counter(COUNTER_CHECKS_DONE).load(Ordering::Acquire) == 1 || is_gone(outread_pid)
    });
    if counter(COUNTER_CHECKS_DONE).load(Ordering::Acquire) != 1 {
        fail("outread exited before finishing its checks");
    }
    wait_until("outread to exit", || is_gone(outread_pid));

    let basic_pid = reported_pid(COUNTER_BASIC_PID, "basic writer");
    let flood_pid = reported_pid(COUNTER_FLOOD_PID, "flood writer");
    let flood_syscall_pid = reported_pid(COUNTER_FLOOD_SYSCALL_PID, "flood syscall writer");
    let orphan_pid = reported_pid(COUNTER_ORPHAN_PID, "orphan writer");
    wait_until("the orphan writer to exit", || is_gone(orphan_pid));
    if counter(COUNTER_ORPHAN_WRITES_DONE).load(Ordering::Acquire) != 1 {
        fail("the orphan writer did not finish its writes");
    }

    let passed = counter(COUNTER_CHECKS_PASSED).load(Ordering::Acquire);
    let failed = counter(COUNTER_CHECKS_FAILED).load(Ordering::Acquire);
    serial_println_core!(
        "test_user_stdout: round {}: {} checks passed, {} failed",
        round,
        passed,
        failed
    );
    if failed != 0 {
        fail("outread counted failed checks");
    }
    if passed != EXPECTED_PASSED_CHECKS {
        fail("outread ran an unexpected number of checks");
    }
    if counter(COUNTER_READER_EXITING).load(Ordering::Acquire) != 1 {
        fail("outread did not signal its exit");
    }

    // outread prints only on a failed check and has no channel, so it makes no Write either
    for (pid, what, expected_write_syscalls) in [
        (outread_pid, OUTREAD_NAME, DIRECT_WRITE_SYSCALLS),
        (basic_pid, "basic writer", BASIC_WRITE_SYSCALLS),
        (flood_pid, "flood writer", DIRECT_WRITE_SYSCALLS),
        (
            flood_syscall_pid,
            "flood syscall writer",
            FLOOD_SYSCALL_WRITE_SYSCALLS,
        ),
        (orphan_pid, "orphan writer", DIRECT_WRITE_SYSCALLS),
    ] {
        check_clean_exit(pid, what, expected_write_syscalls);
    }

    let live_channels = live_channel_count();
    if live_channels != 0 {
        serial_println_core!("{} channels still live", live_channels);
        fail("channels leaked");
    }

    PROCESS_MANAGER.lock().cleanup_dead();
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_stdout: start ===");

    mount_test_disk();
    kernel::process_start::init_userspace();
    bsp_init::wait_for_ap_cores_blocking();

    let elf_info = match load_elf_from_path(OUTREAD_PATH) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("load outread: {:?}", error);
            fail("could not load the outread ELF");
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
        "test_user_stdout: free frames {} before, {} after",
        start_free_frames,
        end_free_frames
    );
    if end_free_frames != start_free_frames {
        fail("free frame count changed over a round");
    }

    serial_println_core!("=== test_user_stdout: ok ===");
    exit_qemu(QemuExitCode::Success);
}
