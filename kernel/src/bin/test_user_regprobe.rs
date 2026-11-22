#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};
use kernel::process::elf_loader::load_elf_from_path;
use kernel::process::process_manager::BIGPROC_PID;
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::{PID, ProcessState};
use kernel::testing::{QemuExitCode, exit_qemu, test_panic_handler};
use kernel::{AP_CORE_COUNT, boot_common, bsp_init, serial_println_core};
use x86_64::instructions::hlt;

// more processes than AP cores forces some of them to share a core
const PROCESS_COUNT: usize = 2 * AP_CORE_COUNT as usize;
const TIMEOUT_MS: u64 = 20000;
const NS_PER_MS: u64 = 1_000_000;
const PROCESS_PRIORITY: u8 = 5;

const COUNTER_INSTANCE_COUNT: usize = 0;
const COUNTER_PROBES_DONE: usize = 1;

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

fn test_main() -> ! {
    serial_println_core!("=== test_user_regprobe: start ===");

    kernel::testing::mount_test_disk();

    kernel::process_start::init_userspace();
    bsp_init::wait_for_ap_cores_blocking();

    let elf_info = match load_elf_from_path("/BIN/regprobe") {
        Ok(info) => info,
        Err(e) => {
            serial_println_core!("FAIL: parse regprobe ELF: {:?}", e);
            exit_qemu(QemuExitCode::Failed);
        }
    };

    // set before any instance exists, every instance waits at the barrier for all of them
    counter(COUNTER_PROBES_DONE).store(0, Ordering::Release);
    counter(COUNTER_INSTANCE_COUNT).store(PROCESS_COUNT as u32, Ordering::Release);

    let mut pids: [PID; PROCESS_COUNT] = [0; PROCESS_COUNT];

    {
        let mut pm = PROCESS_MANAGER.lock();

        for pid_slot in pids.iter_mut() {
            *pid_slot = match pm.create_process_from_elf(
                BIGPROC_PID,
                &elf_info,
                "regprobe",
                PROCESS_PRIORITY,
            ) {
                Ok(pid) => pid,
                Err(e) => {
                    serial_println_core!("FAIL: create_process_from_elf: {:?}", e);
                    exit_qemu(QemuExitCode::Failed);
                }
            };
        }
    }

    let start_ns = kernel::interrupts::system_uptime_ns();
    let mut success = true;

    for pid in pids {
        loop {
            if kernel::interrupts::system_uptime_ns().saturating_sub(start_ns)
                >= TIMEOUT_MS * NS_PER_MS
            {
                serial_println_core!("FAIL: regprobe pid={} timed out", pid);
                exit_qemu(QemuExitCode::Failed);
            }

            let state = {
                let pm = PROCESS_MANAGER.lock();
                pm.get_process(pid).ok().map(|p| (p.state, p.exit_code))
            };

            match state {
                Some((ProcessState::Terminated, exit_code)) => {
                    if exit_code != Some(0) {
                        serial_println_core!(
                            "FAIL: regprobe pid={} exit code {:?}, expected Some(0)",
                            pid,
                            exit_code
                        );
                        success = false;
                    }
                    break;
                }
                Some(_) => {}
                None => {
                    serial_println_core!("FAIL: regprobe pid={} vanished", pid);
                    success = false;
                    break;
                }
            }

            hlt();
        }
    }

    if success {
        serial_println_core!("=== test_user_regprobe: ok ===");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println_core!("=== test_user_regprobe: FAILED ===");
        exit_qemu(QemuExitCode::Failed);
    }
}
