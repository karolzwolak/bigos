#![no_std]
#![no_main]

extern crate alloc;

use kernel::process::elf_loader::{ElfLoadInfo, FPUSTRESS_ELF};
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::{PID, ProcessState};
use kernel::testing::{QemuExitCode, exit_qemu, test_panic_handler};
use kernel::{AP_CORE_COUNT, boot_common, bsp_init, serial_println_core};
use x86_64::instructions::hlt;

// more processes than AP cores forces some of them to share a core
const PROCESS_COUNT: usize = 2 * AP_CORE_COUNT as usize;
const TIMEOUT_MS: u64 = 20000;
const NS_PER_MS: u64 = 1_000_000;
const PROCESS_PRIORITY: u8 = 5;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    unsafe { boot_common::bsp_early_init() };
    test_main();
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_fpu_switch: start ===");

    kernel::process::shared_state::init_shared_state();
    bsp_init::wait_for_ap_cores_blocking();

    let elf_info = match ElfLoadInfo::from_elf_data(FPUSTRESS_ELF) {
        Ok(info) => info,
        Err(e) => {
            serial_println_core!("FAIL: parse fpustress ELF: {:?}", e);
            exit_qemu(QemuExitCode::Failed);
        }
    };

    let mut pids: [PID; PROCESS_COUNT] = [0; PROCESS_COUNT];

    {
        let mut pm = PROCESS_MANAGER.lock();

        for pid_slot in pids.iter_mut() {
            *pid_slot =
                match pm.create_process_from_elf(0, &elf_info, "fpustress", PROCESS_PRIORITY) {
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
                serial_println_core!("FAIL: fpustress pid={} timed out", pid);
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
                            "FAIL: fpustress pid={} exit code {:?}, expected Some(0)",
                            pid,
                            exit_code
                        );
                        success = false;
                    }
                    break;
                }
                Some(_) => {}
                None => {
                    serial_println_core!("FAIL: fpustress pid={} vanished", pid);
                    success = false;
                    break;
                }
            }

            hlt();
        }
    }

    if success {
        serial_println_core!("=== test_user_fpu_switch: ok ===");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println_core!("=== test_user_fpu_switch: FAILED ===");
        exit_qemu(QemuExitCode::Failed);
    }
}
