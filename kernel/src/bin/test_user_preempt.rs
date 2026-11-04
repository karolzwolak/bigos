#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use core::sync::atomic::Ordering;
use kernel::process::elf_loader::{ElfLoadInfo, SPINNER_ELF};
use kernel::process::process::{PID, ProcessState};
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::shared_state::get_shared_program_data_buffer_mut;
use kernel::{AP_CORE_COUNT, boot_common, bsp_init, serial_println_core};
use x86_64::instructions::{hlt, port::Port};

// one more spinner than AP cores, so some core has to take turns between two of them
const PROCESS_COUNT: usize = AP_CORE_COUNT as usize + 1;

// indexes into the shared test counters, the spinner program uses the same values
const COUNTER_SPINNER_COUNT: usize = 1;
const TIMEOUT_MS: u64 = 20000;
const NS_PER_MS: u64 = 1_000_000;
const PROCESS_PRIORITY: u8 = 5;

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

fn test_main() -> ! {
    serial_println_core!("=== test_user_preempt: start ===");

    kernel::process::shared_state::init_shared_state();
    bsp_init::wait_for_ap_cores_blocking();

    let elf_info = match ElfLoadInfo::from_elf_data(SPINNER_ELF) {
        Ok(info) => info,
        Err(e) => {
            serial_println_core!("FAIL: parse spinner ELF: {:?}", e);
            exit_qemu(QemuExitCode::Failed);
        }
    };

    let mut pids: [PID; PROCESS_COUNT] = [0; PROCESS_COUNT];

    // SAFETY: no process exists yet, so nothing else touches the page
    unsafe {
        get_shared_program_data_buffer_mut().test_counters[COUNTER_SPINNER_COUNT]
            .store(PROCESS_COUNT as u32, Ordering::Release);
    }

    {
        let mut pm = PROCESS_MANAGER.lock();

        for pid_slot in pids.iter_mut() {
            *pid_slot = match pm.create_process_from_elf(0, &elf_info, "spinner", PROCESS_PRIORITY)
            {
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
                serial_println_core!("FAIL: spinner pid={} timed out", pid);
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
                            "FAIL: spinner pid={} exit code {:?}, expected Some(0)",
                            pid,
                            exit_code
                        );
                        success = false;
                    }
                    break;
                }
                Some(_) => {}
                None => {
                    serial_println_core!("FAIL: spinner pid={} vanished", pid);
                    success = false;
                    break;
                }
            }

            hlt();
        }
    }

    if success {
        serial_println_core!("=== test_user_preempt: ok ===");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println_core!("=== test_user_preempt: FAILED ===");
        exit_qemu(QemuExitCode::Failed);
    }
}
