#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use core::sync::atomic::Ordering;
use kernel::process::CORE_POOL;
use kernel::{AP_CORES_READY, MAX_CORES, boot_common, bsp_init, serial_println_core};
use x86_64::instructions::{hlt, port::Port};

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
    serial_println_core!("=== test_smp_boot: start ===");
    serial_println_core!(
        "test_smp_boot: MAX_CORES={} AP_CORE_COUNT={}",
        MAX_CORES,
        kernel::AP_CORE_COUNT
    );

    let total_cores = CORE_POOL.lock().total_cores();
    let expected_ap = total_cores.saturating_sub(1);
    serial_println_core!(
        "test_smp_boot: CORE_POOL total_cores={} expected_ap={}",
        total_cores,
        expected_ap
    );
    serial_println_core!(
        "test_smp_boot: AP_CORES_READY initial={}",
        AP_CORES_READY.load(Ordering::Acquire)
    );

    const TIMEOUT_MS: u64 = 5000;
    let ok = bsp_init::wait_for_ap_cores_with_timeout(TIMEOUT_MS);
    let (ready, expected) = bsp_init::check_ap_cores_ready();
    serial_println_core!(
        "test_smp_boot: after wait ready={} expected={} ok={}",
        ready,
        expected,
        ok
    );

    let mut success = true;

    if !ok {
        serial_println_core!(
            "FAIL: AP cores did not become ready within {} ms",
            TIMEOUT_MS
        );
        success = false;
    }

    if ready != expected {
        serial_println_core!("FAIL: ready {} != expected {}", ready, expected);
        success = false;
    }

    if total_cores as u8 != MAX_CORES {
        serial_println_core!(
            "INFO: total_cores {} != MAX_CORES {} (QEMU smp may be less than max)",
            total_cores,
            MAX_CORES
        );
    }

    if total_cores != (expected as u8 + 1) && expected != 0 {
        serial_println_core!("FAIL: total {} != expected+1 {}", total_cores, expected + 1);
        success = false;
    }

    if total_cores == 0 || total_cores > MAX_CORES {
        serial_println_core!(
            "FAIL: total_cores {} out of range 1..={}",
            total_cores,
            MAX_CORES
        );
        success = false;
    }

    if expected == 0 && total_cores == 1 {
        serial_println_core!("WARN: single-core boot, no APs to test");
    }

    if success {
        serial_println_core!(
            "PASS: all {} AP cores reported ready (total {} cores)",
            expected,
            total_cores
        );
        serial_println_core!("=== test_smp_boot: ok ===");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println_core!("=== test_smp_boot: FAILED ===");
        exit_qemu(QemuExitCode::Failed);
    }
}
