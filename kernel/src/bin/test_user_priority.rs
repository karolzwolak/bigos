#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use core::sync::atomic::Ordering;
use kernel::process::elf_loader::{BUSYLOOP_ELF, ElfLoadInfo, WORKER_ELF, YIELDLOOP_ELF};
use kernel::process::process::PID;
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::task;
use kernel::{AP_CORE_COUNT, boot_common, bsp_init, serial_println_core};
use x86_64::instructions::{hlt, port::Port};

// indexes into the shared test counters, the user programs use the same values
const COUNTER_RUNNING: usize = 0;
const COUNTER_STOP: usize = 1;
const COUNTER_YIELDS: usize = 2;
const COUNTER_WORKERS_DONE: usize = 3;

const HIGH_PRIORITY: u8 = 7;
const LOW_PRIORITY: u8 = 1;

// one high priority task per AP core keeps every core busy, one worker per core has to squeeze in
const HIGH_TASK_COUNT: usize = AP_CORE_COUNT as usize;
const WORKER_COUNT: usize = AP_CORE_COUNT as usize;

const DEADLINE_US: u64 = 15_000_000;
const STARTUP_DEADLINE_US: u64 = 5_000_000;
const MIN_YIELDS_PER_HIGH_TASK: u32 = 1;

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
    serial_println_core!("=== test_user_priority: FAILED ===");
    exit_qemu(QemuExitCode::Failed);
}

fn counter(index: usize) -> &'static core::sync::atomic::AtomicU32 {
    debug_assert!(index < TEST_COUNTER_COUNT);

    // SAFETY: the shared page lives forever and the counters are atomics
    unsafe { &get_shared_program_data_buffer_mut().test_counters[index] }
}

fn reset_counters() {
    for index in 0..TEST_COUNTER_COUNT {
        counter(index).store(0, Ordering::Release);
    }
}

fn spawn(elf_info: &ElfLoadInfo, name: &str, priority: u8) -> PID {
    let mut process_manager = PROCESS_MANAGER.lock();

    match process_manager.create_process_from_elf(0, elf_info, name, priority) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create a process");
        }
    }
}

fn wait_until(what: &str, deadline_us: u64, condition: impl Fn() -> bool) {
    let start_us = kernel::interrupts::tsc_timestamp_us();

    while !condition() {
        if kernel::interrupts::tsc_timestamp_us() - start_us > deadline_us {
            serial_println_core!("timed out waiting for: {}", what);
            fail("timeout");
        }
        core::hint::spin_loop();
    }
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

/// Fills every AP core with high priority tasks that run until told to stop, then
/// expects the low priority workers to finish anyway
fn run_case(
    case_name: &str,
    high_elf: &ElfLoadInfo,
    high_name: &str,
    worker_elf: &ElfLoadInfo,
    high_tasks_yield: bool,
) {
    serial_println_core!("test_user_priority: case {}", case_name);
    reset_counters();

    let mut pids = [0 as PID; HIGH_TASK_COUNT + WORKER_COUNT];

    for pid_slot in pids[..HIGH_TASK_COUNT].iter_mut() {
        *pid_slot = spawn(high_elf, high_name, HIGH_PRIORITY);
    }
    wait_until("high priority tasks to start", STARTUP_DEADLINE_US, || {
        counter(COUNTER_RUNNING).load(Ordering::Acquire) as usize == HIGH_TASK_COUNT
    });

    for pid_slot in pids[HIGH_TASK_COUNT..].iter_mut() {
        *pid_slot = spawn(worker_elf, "worker", LOW_PRIORITY);
    }

    let start_us = kernel::interrupts::tsc_timestamp_us();
    wait_until("low priority workers to finish", DEADLINE_US, || {
        counter(COUNTER_WORKERS_DONE).load(Ordering::Acquire) as usize == WORKER_COUNT
    });
    serial_println_core!(
        "test_user_priority: workers finished {} us after they were created",
        kernel::interrupts::tsc_timestamp_us() - start_us
    );

    if high_tasks_yield {
        let yields = counter(COUNTER_YIELDS).load(Ordering::Acquire);
        serial_println_core!("test_user_priority: high priority yields {}", yields);
        if yields < MIN_YIELDS_PER_HIGH_TASK * HIGH_TASK_COUNT as u32 {
            fail("yielding high priority tasks made no progress");
        }
    }

    counter(COUNTER_STOP).store(1, Ordering::Release);
    for pid in pids {
        wait_until("tasks to exit", DEADLINE_US, || {
            task::find_slot_by_pid(pid).is_none()
        });
    }

    PROCESS_MANAGER.lock().cleanup_dead();
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_priority: start ===");

    kernel::process::shared_state::init_shared_state();
    bsp_init::wait_for_ap_cores_blocking();

    let yieldloop_elf = parse(YIELDLOOP_ELF);
    let busyloop_elf = parse(BUSYLOOP_ELF);
    let worker_elf = parse(WORKER_ELF);

    run_case("yielding", &yieldloop_elf, "yieldloop", &worker_elf, true);
    run_case("spinning", &busyloop_elf, "busyloop", &worker_elf, false);

    serial_println_core!("=== test_user_priority: ok ===");
    exit_qemu(QemuExitCode::Success);
}
