#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use core::sync::atomic::Ordering;
use kernel::process::elf_loader::{BUSYLOOP_ELF, ElfLoadInfo};
use kernel::process::process::{PID, ProcessState};
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::get_shared_program_data_buffer_mut;
use kernel::process::task::{self, MAX_TASKS, TaskState};
use kernel::{boot_common, bsp_init, serial_println_core};
use x86_64::instructions::{hlt, port::Port};

// indexes into the shared test counters, the busyloop program uses the same values
const COUNTER_RUNNING: usize = 0;

const BUSYLOOP_PRIORITY: u8 = 5;
const KILL_EXIT_CODE: i32 = 77;
const KILL_DEADLINE_US: u64 = 1_000_000;
const STARTUP_DEADLINE_US: u64 = 5_000_000;
const STRESS_ROUNDS: u32 = 200;

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
    serial_println_core!("=== test_user_kill: FAILED ===");
    exit_qemu(QemuExitCode::Failed);
}

fn set_running_counter(value: u32) {
    // SAFETY: only the test core writes it while no busyloop process exists
    unsafe {
        get_shared_program_data_buffer_mut().test_counters[COUNTER_RUNNING]
            .store(value, Ordering::Release);
    }
}

fn running_counter() -> u32 {
    // SAFETY: the page lives forever
    unsafe {
        get_shared_program_data_buffer_mut().test_counters[COUNTER_RUNNING].load(Ordering::Acquire)
    }
}

fn spawn_busyloop(elf_info: &ElfLoadInfo) -> PID {
    let mut process_manager = PROCESS_MANAGER.lock();

    match process_manager.create_process_from_elf(0, elf_info, "busyloop", BUSYLOOP_PRIORITY) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create busyloop");
        }
    }
}

fn wait_until_running(deadline_us: u64) {
    let start_us = kernel::interrupts::tsc_timestamp_us();

    while running_counter() == 0 {
        if kernel::interrupts::tsc_timestamp_us() - start_us > deadline_us {
            fail("busyloop never started running");
        }
        core::hint::spin_loop();
    }
}

/// Returns how long the task took to disappear, its slot is released after it died
fn kill_and_wait(pid: PID) -> u64 {
    let start_us = kernel::interrupts::tsc_timestamp_us();

    if !task::request_kill(pid, KILL_EXIT_CODE) {
        fail("request_kill found no task");
    }

    while task::find_slot_by_pid(pid).is_some() {
        if kernel::interrupts::tsc_timestamp_us() - start_us > KILL_DEADLINE_US {
            fail("killed task still alive after the deadline");
        }
        core::hint::spin_loop();
    }

    let elapsed_us = kernel::interrupts::tsc_timestamp_us() - start_us;

    let process_manager = PROCESS_MANAGER.lock();
    match process_manager.get_process(pid) {
        Ok(process) => {
            if process.state != ProcessState::Terminated
                || process.exit_code != Some(KILL_EXIT_CODE)
            {
                serial_println_core!(
                    "pid {} state {:?} exit {:?}",
                    pid,
                    process.state,
                    process.exit_code
                );
                fail("killed process is not Terminated with the requested exit code");
            }
        }
        Err(_) => fail("killed process vanished before its exit code was read"),
    }

    elapsed_us
}

fn free_task_slot_count() -> usize {
    task::TASKS
        .iter()
        .filter(|slot| slot.state() == TaskState::Free)
        .count()
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_kill: start ===");

    kernel::process::shared_state::init_shared_state();
    bsp_init::wait_for_ap_cores_blocking();

    let elf_info = match ElfLoadInfo::from_elf_data(BUSYLOOP_ELF) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("parse busyloop ELF: {:?}", error);
            fail("could not parse busyloop ELF");
        }
    };

    // a spinner on another core never enters the kernel, only the request can stop it
    set_running_counter(0);
    let pid = spawn_busyloop(&elf_info);
    wait_until_running(STARTUP_DEADLINE_US);

    let slot_index = task::find_slot_by_pid(pid).expect("running task has a slot");
    let home_core = task::slot(slot_index).home_core.load(Ordering::Relaxed);
    if home_core == 0 {
        fail("busyloop was placed on the boot core");
    }

    let kill_us = kill_and_wait(pid);
    serial_println_core!(
        "test_user_kill: spinner on core {} gone {} us after the request",
        home_core,
        kill_us
    );

    // one more round settles the frame allocator, the first process may take frames
    // from the bump region that later end up on the free list
    set_running_counter(0);
    let warmup_pid = spawn_busyloop(&elf_info);
    wait_until_running(STARTUP_DEADLINE_US);
    kill_and_wait(warmup_pid);
    PROCESS_MANAGER.lock().cleanup_dead();

    let start_free_frames = kernel::memory::get_frame_allocator().free_frame_count();
    let start_free_slots = free_task_slot_count();
    serial_println_core!(
        "test_user_kill: stress start, free frames {} free slots {}",
        start_free_frames,
        start_free_slots
    );

    for round in 0..STRESS_ROUNDS {
        set_running_counter(0);
        let round_pid = spawn_busyloop(&elf_info);

        // even rounds kill a task that may not have run at all
        if round % 2 == 1 {
            wait_until_running(STARTUP_DEADLINE_US);
        }

        kill_and_wait(round_pid);
        PROCESS_MANAGER.lock().cleanup_dead();
    }

    let end_free_frames = kernel::memory::get_frame_allocator().free_frame_count();
    let end_free_slots = free_task_slot_count();
    serial_println_core!(
        "test_user_kill: stress end, free frames {} free slots {} of {}",
        end_free_frames,
        end_free_slots,
        MAX_TASKS
    );

    if end_free_frames != start_free_frames {
        fail("free frame count changed over the spawn and kill loop");
    }
    if end_free_slots != start_free_slots {
        fail("task slots leaked over the spawn and kill loop");
    }
    if kernel::memory::get_frame_allocator().free_list_has_duplicates() {
        fail("a frame was freed more than once");
    }

    serial_println_core!("=== test_user_kill: ok ===");
    exit_qemu(QemuExitCode::Success);
}
