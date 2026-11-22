#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};
use kernel::process::elf_loader::load_elf_from_path;
use kernel::process::process_manager::BIGPROC_PID;
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::thread::{self, ThreadState};
use kernel::process::{ChildPolicy, PID, ProcessState};
use kernel::testing::{QemuExitCode, exit_qemu, test_panic_handler};
use kernel::{AP_CORE_COUNT, boot_common, bsp_init, serial_println_core};

// indexes into the shared test counters, the busyloop program uses the same values
const COUNTER_RUNNING: usize = 0;
const COUNTER_STOP: usize = 1;

const PROGRAM_PRIORITY: u8 = 5;
const KILL_EXIT_CODE: i32 = 55;

// the placement at creation spreads these evenly, two per AP core
const THREADS_PER_CORE: usize = 2;
const THREAD_COUNT: usize = THREADS_PER_CORE * AP_CORE_COUNT as usize;
// the core whose threads are killed, so it goes idle while the others still hold two
const EMPTIED_CORE: u8 = 1;

const STARTUP_DEADLINE_US: u64 = 10_000_000;
const MIGRATION_DEADLINE_US: u64 = 5_000_000;
const EXIT_DEADLINE_US: u64 = 5_000_000;

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
    serial_println_core!("=== test_user_steal: FAILED ===");
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

fn assigned_core_of(pid: PID) -> Option<u8> {
    let slot_index = thread::find_slot_by_pid(pid)?;

    Some(
        thread::slot(slot_index)
            .assigned_core
            .load(Ordering::Relaxed),
    )
}

fn total_core_migration_count(pids: &[PID]) -> u32 {
    pids.iter()
        .filter_map(|pid| thread::find_slot_by_pid(*pid))
        .map(|slot_index| {
            thread::slot(slot_index)
                .core_migration_count
                .load(Ordering::Relaxed)
        })
        .sum()
}

fn exit_code_of(pid: PID) -> Option<i32> {
    let process_manager = PROCESS_MANAGER.lock();
    let process = process_manager.get_process(pid).ok()?;
    if process.state != ProcessState::Terminated {
        return None;
    }

    process.exit_code
}

fn free_thread_slots() -> usize {
    thread::THREADS
        .iter()
        .filter(|slot| slot.state() == ThreadState::Free)
        .count()
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_steal: start ===");

    kernel::testing::mount_test_disk();

    kernel::process_start::init_userspace();
    bsp_init::wait_for_ap_cores_blocking();

    // bigproc keeps its slot for good, so leaks are measured against this
    let baseline_free_slots = free_thread_slots();

    let elf_info = match load_elf_from_path("/BIN/busyloop") {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("parse busyloop ELF: {:?}", error);
            fail("could not parse busyloop ELF");
        }
    };

    for index in 0..TEST_COUNTER_COUNT {
        counter(index).store(0, Ordering::Release);
    }

    let mut pids = [0 as PID; THREAD_COUNT];
    for pid_slot in pids.iter_mut() {
        let mut process_manager = PROCESS_MANAGER.lock();
        *pid_slot = match process_manager.create_process_from_elf(
            BIGPROC_PID,
            &elf_info,
            "busyloop",
            PROGRAM_PRIORITY,
        ) {
            Ok(pid) => pid,
            Err(error) => {
                serial_println_core!("create_process_from_elf: {:?}", error);
                fail("could not create a process");
            }
        };
    }

    // two per core run in turns, so the second one starts after a time slice
    wait_until("every thread to run once", STARTUP_DEADLINE_US, || {
        counter(COUNTER_RUNNING).load(Ordering::Acquire) as usize == THREAD_COUNT
    });

    for core_id in 1..=AP_CORE_COUNT {
        let on_core = pids
            .iter()
            .filter(|pid| assigned_core_of(**pid) == Some(core_id))
            .count();
        serial_println_core!(
            "test_user_steal: core {} holds {} threads",
            core_id,
            on_core
        );
        if on_core != THREADS_PER_CORE {
            fail("threads were not spread evenly at creation");
        }
    }

    // idle core 1 while the other cores keep a thread waiting behind the running one
    let mut survivors = [0 as PID; THREAD_COUNT];
    let mut survivor_count = 0;
    for pid in pids {
        if assigned_core_of(pid) == Some(EMPTIED_CORE) {
            if !thread::request_kill(pid, KILL_EXIT_CODE, ChildPolicy::KillChildren) {
                fail("request_kill found no thread");
            }
        } else {
            survivors[survivor_count] = pid;
            survivor_count += 1;
        }
    }
    let survivors = &survivors[..survivor_count];

    for pid in pids {
        if !survivors.contains(&pid) {
            wait_until("a killed thread to exit", EXIT_DEADLINE_US, || {
                thread::find_slot_by_pid(pid).is_none()
            });
        }
    }

    wait_until(
        "an idle core to steal a thread",
        MIGRATION_DEADLINE_US,
        || total_core_migration_count(survivors) > 0,
    );
    serial_println_core!(
        "test_user_steal: {} core_migration_count among {} surviving threads",
        total_core_migration_count(survivors),
        survivors.len()
    );

    counter(COUNTER_STOP).store(1, Ordering::Release);
    for pid in survivors {
        wait_until("a thread to finish", EXIT_DEADLINE_US, || {
            thread::find_slot_by_pid(*pid).is_none()
        });
        if exit_code_of(*pid) != Some(0) {
            fail("a migrated thread did not finish cleanly");
        }
    }

    for pid in pids {
        if !survivors.contains(&pid) && exit_code_of(pid) != Some(KILL_EXIT_CODE) {
            fail("a killed thread has the wrong exit code");
        }
    }

    if free_thread_slots() != baseline_free_slots {
        fail("thread slots leaked");
    }

    serial_println_core!("=== test_user_steal: ok ===");
    exit_qemu(QemuExitCode::Success);
}
