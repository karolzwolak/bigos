#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};
use kernel::process::elf_loader::{ElfLoadInfo, PINGPONG_ELF, SLEEPER_ELF};
use kernel::process::process::{PID, ProcessState};
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::task::{self, TaskState};
use kernel::{boot_common, bsp_init, serial_println_core};
use x86_64::instructions::{hlt, port::Port};

// indexes into the shared test counters, the user programs use the same values
const COUNTER_RUNNING: usize = 0;
const COUNTER_SLEEP_MS: usize = 4;
const COUNTER_SLEEPING_TASKS_DONE: usize = 5;
const COUNTER_PING_ROUNDS_BASE: usize = 8;

const PROGRAM_PRIORITY: u8 = 5;
const PING_ROUNDS: u32 = 2000;
const PING_ROLE_COUNT: usize = 2;

const SLEEP_MS: u32 = 100;
const MICROSECONDS_PER_MILLISECOND: u64 = 1000;
// a task that spun instead of sleeping would collect about ten ticks in 100 ms
const MAX_CPU_TICKS_WHILE_SLEEPING: u64 = 3;

const LONG_SLEEP_MS: u32 = 30_000;
const KILL_DEADLINE_US: u64 = 1_000_000;
const KILL_EXIT_CODE: i32 = 66;
const DEADLINE_US: u64 = 15_000_000;
const STARTUP_DEADLINE_US: u64 = 5_000_000;

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
    serial_println_core!("=== test_user_sleep: FAILED ===");
    exit_qemu(QemuExitCode::Failed);
}

fn counter(index: usize) -> &'static AtomicU32 {
    debug_assert!(index < TEST_COUNTER_COUNT);

    // SAFETY: the shared page lives forever and the counters are atomics
    unsafe { &get_shared_program_data_buffer_mut().test_counters[index] }
}

fn reset_counters() {
    for index in 0..TEST_COUNTER_COUNT {
        counter(index).store(0, Ordering::Release);
    }
}

fn now_us() -> u64 {
    kernel::interrupts::tsc_timestamp_us()
}

fn parse(elf: &[u8]) -> ElfLoadInfo {
    match ElfLoadInfo::from_elf_data(elf) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("parse ELF: {:?}", error);
            fail("could not parse an ELF");
        }
    }
}

fn spawn(elf_info: &ElfLoadInfo, name: &str) -> PID {
    let mut process_manager = PROCESS_MANAGER.lock();

    match process_manager.create_process_from_elf(0, elf_info, name, PROGRAM_PRIORITY) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create a process");
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

fn wait_until_gone(pid: PID, deadline_us: u64) {
    wait_until("a task to exit", deadline_us, || {
        task::find_slot_by_pid(pid).is_none()
    });
}

fn exit_code_of(pid: PID) -> Option<i32> {
    let process_manager = PROCESS_MANAGER.lock();
    let process = process_manager.get_process(pid).ok()?;
    if process.state != ProcessState::Terminated {
        return None;
    }

    process.exit_code
}

fn test_sleep_duration_and_cpu_use(sleeper_elf: &ElfLoadInfo) {
    serial_println_core!("test_user_sleep: sleep {} ms", SLEEP_MS);
    reset_counters();
    counter(COUNTER_SLEEP_MS).store(SLEEP_MS, Ordering::Release);

    let start_us = now_us();
    let pid = spawn(sleeper_elf, "sleeper");
    let slot_index = loop {
        if let Some(slot_index) = task::find_slot_by_pid(pid) {
            break slot_index;
        }
        core::hint::spin_loop();
    };

    // the slot is released with the task, so the highest tick count is sampled while it lives
    let mut max_cpu_ticks = 0;
    while task::find_slot_by_pid(pid).is_some() {
        if now_us() - start_us > DEADLINE_US {
            fail("sleeper did not finish");
        }

        max_cpu_ticks = max_cpu_ticks.max(task::slot(slot_index).cpu_ticks.load(Ordering::Relaxed));
        core::hint::spin_loop();
    }
    let elapsed_us = now_us() - start_us;

    serial_println_core!(
        "test_user_sleep: sleeper took {} us and used at most {} cpu ticks",
        elapsed_us,
        max_cpu_ticks
    );

    if elapsed_us < SLEEP_MS as u64 * MICROSECONDS_PER_MILLISECOND {
        fail("process ended before the sleep time passed");
    }
    if exit_code_of(pid) != Some(0) {
        fail("sleeper measured a sleep shorter than requested");
    }
    if counter(COUNTER_SLEEPING_TASKS_DONE).load(Ordering::Acquire) != 1 {
        fail("sleeper did not report completion");
    }
    if max_cpu_ticks > MAX_CPU_TICKS_WHILE_SLEEPING {
        fail("sleeping task burned cpu time");
    }
}

fn test_kill_during_sleep(sleeper_elf: &ElfLoadInfo) {
    serial_println_core!("test_user_sleep: kill during a {} ms sleep", LONG_SLEEP_MS);
    reset_counters();
    counter(COUNTER_SLEEP_MS).store(LONG_SLEEP_MS, Ordering::Release);

    let pid = spawn(sleeper_elf, "sleeper");
    wait_until("the sleeper to start", STARTUP_DEADLINE_US, || {
        counter(COUNTER_RUNNING).load(Ordering::Acquire) == 1
    });

    let slot_index = task::find_slot_by_pid(pid).expect("a running task has a slot");
    wait_until("the sleeper to block", STARTUP_DEADLINE_US, || {
        task::slot(slot_index).state() == TaskState::Blocked
    });

    let start_us = now_us();
    if !task::request_kill(pid, KILL_EXIT_CODE) {
        fail("request_kill found no task");
    }
    wait_until_gone(pid, KILL_DEADLINE_US);
    serial_println_core!(
        "test_user_sleep: sleeper gone {} us after the kill request",
        now_us() - start_us
    );

    if exit_code_of(pid) != Some(KILL_EXIT_CODE) {
        fail("killed sleeper has the wrong exit code");
    }
}

fn test_wake_racing_with_block(pingpong_elf: &ElfLoadInfo) {
    serial_println_core!("test_user_sleep: {} ping pong rounds", PING_ROUNDS);
    reset_counters();

    let first = spawn(pingpong_elf, "pingpong");
    let second = spawn(pingpong_elf, "pingpong");

    let first_slot = task::find_slot_by_pid(first).expect("a new task has a slot");
    let second_slot = task::find_slot_by_pid(second).expect("a new task has a slot");
    let first_core = task::slot(first_slot).home_core.load(Ordering::Relaxed);
    let second_core = task::slot(second_slot).home_core.load(Ordering::Relaxed);
    if first_core == second_core {
        fail("both ping pong tasks share a core, so no wake crosses cores");
    }

    let start_us = now_us();
    wait_until_gone(first, DEADLINE_US);
    wait_until_gone(second, DEADLINE_US);
    serial_println_core!("test_user_sleep: ping pong took {} us", now_us() - start_us);

    for role in 0..PING_ROLE_COUNT {
        let rounds = counter(COUNTER_PING_ROUNDS_BASE + role).load(Ordering::Acquire);
        if rounds != PING_ROUNDS {
            serial_println_core!("role {} played {} rounds", role, rounds);
            fail("a ping pong role missed a round");
        }
    }
    if exit_code_of(first) != Some(0) || exit_code_of(second) != Some(0) {
        fail("a ping pong task did not exit cleanly");
    }
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_sleep: start ===");

    kernel::process::shared_state::init_shared_state();
    bsp_init::wait_for_ap_cores_blocking();

    let sleeper_elf = parse(SLEEPER_ELF);
    let pingpong_elf = parse(PINGPONG_ELF);

    test_sleep_duration_and_cpu_use(&sleeper_elf);
    test_kill_during_sleep(&sleeper_elf);
    test_wake_racing_with_block(&pingpong_elf);

    PROCESS_MANAGER.lock().cleanup_dead();

    serial_println_core!("=== test_user_sleep: ok ===");
    exit_qemu(QemuExitCode::Success);
}
