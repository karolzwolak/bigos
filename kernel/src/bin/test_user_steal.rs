#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};
use kernel::process::elf_loader::{BUSYLOOP_ELF, ElfLoadInfo};
use kernel::process::process::{PID, ProcessState};
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::task::{self, MAX_TASKS, TaskState};
use kernel::{AP_CORE_COUNT, boot_common, bsp_init, serial_println_core};
use x86_64::instructions::{hlt, port::Port};

// indexes into the shared test counters, the busyloop program uses the same values
const COUNTER_RUNNING: usize = 0;
const COUNTER_STOP: usize = 1;

const PROGRAM_PRIORITY: u8 = 5;
const KILL_EXIT_CODE: i32 = 55;

// the placement at creation spreads these evenly, two per AP core
const TASKS_PER_CORE: usize = 2;
const TASK_COUNT: usize = TASKS_PER_CORE * AP_CORE_COUNT as usize;
// the core whose tasks are killed, so it goes idle while the others still hold two
const EMPTIED_CORE: u8 = 1;

const STARTUP_DEADLINE_US: u64 = 10_000_000;
const MIGRATION_DEADLINE_US: u64 = 5_000_000;
const EXIT_DEADLINE_US: u64 = 5_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    unsafe {
        let mut port = Port::new(0xF4);
        port.write(exit_code as u32);
    }
    loop {
        hlt();
    }
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    serial_println_core!("PANIC: {:#?}", info);
    exit_qemu(QemuExitCode::Failed);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kmain() -> ! {
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

fn home_core_of(pid: PID) -> Option<u8> {
    let slot_index = task::find_slot_by_pid(pid)?;

    Some(task::slot(slot_index).home_core.load(Ordering::Relaxed))
}

fn total_core_migration_count(pids: &[PID]) -> u32 {
    pids.iter()
        .filter_map(|pid| task::find_slot_by_pid(*pid))
        .map(|slot_index| {
            task::slot(slot_index)
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

fn test_main() -> ! {
    serial_println_core!("=== test_user_steal: start ===");

    kernel::process::shared_state::init_shared_state();
    bsp_init::wait_for_ap_cores_blocking();

    let elf_info = match ElfLoadInfo::from_elf_data(BUSYLOOP_ELF) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("parse busyloop ELF: {:?}", error);
            fail("could not parse busyloop ELF");
        }
    };

    for index in 0..TEST_COUNTER_COUNT {
        counter(index).store(0, Ordering::Release);
    }

    let mut pids = [0 as PID; TASK_COUNT];
    for pid_slot in pids.iter_mut() {
        let mut process_manager = PROCESS_MANAGER.lock();
        *pid_slot = match process_manager.create_process_from_elf(
            0,
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
    wait_until("every task to run once", STARTUP_DEADLINE_US, || {
        counter(COUNTER_RUNNING).load(Ordering::Acquire) as usize == TASK_COUNT
    });

    for core_id in 1..=AP_CORE_COUNT {
        let on_core = pids
            .iter()
            .filter(|pid| home_core_of(**pid) == Some(core_id))
            .count();
        serial_println_core!("test_user_steal: core {} holds {} tasks", core_id, on_core);
        if on_core != TASKS_PER_CORE {
            fail("tasks were not spread evenly at creation");
        }
    }

    // idle core 1 while the other cores keep a task waiting behind the running one
    let mut survivors = [0 as PID; TASK_COUNT];
    let mut survivor_count = 0;
    for pid in pids {
        if home_core_of(pid) == Some(EMPTIED_CORE) {
            if !task::request_kill(pid, KILL_EXIT_CODE) {
                fail("request_kill found no task");
            }
        } else {
            survivors[survivor_count] = pid;
            survivor_count += 1;
        }
    }
    let survivors = &survivors[..survivor_count];

    for pid in pids {
        if !survivors.contains(&pid) {
            wait_until("a killed task to exit", EXIT_DEADLINE_US, || {
                task::find_slot_by_pid(pid).is_none()
            });
        }
    }

    wait_until(
        "an idle core to steal a task",
        MIGRATION_DEADLINE_US,
        || total_core_migration_count(survivors) > 0,
    );
    serial_println_core!(
        "test_user_steal: {} core_migration_count among {} surviving tasks",
        total_core_migration_count(survivors),
        survivors.len()
    );

    counter(COUNTER_STOP).store(1, Ordering::Release);
    for pid in survivors {
        wait_until("a task to finish", EXIT_DEADLINE_US, || {
            task::find_slot_by_pid(*pid).is_none()
        });
        if exit_code_of(*pid) != Some(0) {
            fail("a migrated task did not finish cleanly");
        }
    }

    for pid in pids {
        if !survivors.contains(&pid) && exit_code_of(pid) != Some(KILL_EXIT_CODE) {
            fail("a killed task has the wrong exit code");
        }
    }

    let free_slots = task::TASKS
        .iter()
        .filter(|slot| slot.state() == TaskState::Free)
        .count();
    if free_slots != MAX_TASKS {
        fail("task slots leaked");
    }

    serial_println_core!("=== test_user_steal: ok ===");
    exit_qemu(QemuExitCode::Success);
}
