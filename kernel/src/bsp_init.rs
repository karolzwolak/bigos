use crate::{
    AP_CORES_READY, MAX_CORES,
    filesystem::init_filesystem_ata,
    process::{CORE_POOL, shared_state::init_shared_state},
    serial_println_core,
};
use core::sync::atomic::Ordering;
use x86_64::instructions::hlt;

const AP_READY_TIMEOUT_TICKS: u64 = 500;
const AP_READY_POLL_INTERVAL_MS: u64 = 10;

pub fn init_shared_state_on_bsp() {
    init_shared_state();
    serial_println_core!("bsp_init: shared state initialized");
}

pub fn init_filesystem_on_bsp() -> Result<(), &'static str> {
    match init_filesystem_ata() {
        Ok(()) => {
            serial_println_core!("bsp_init: filesystem initialized from ATA drive");
            Ok(())
        }
        Err(e) => {
            serial_println_core!("bsp_init: filesystem init failed: {}", e);
            Err(e)
        }
    }
}

pub fn wait_for_ap_cores_blocking() {
    let core_count = CORE_POOL.lock().total_cores();
    let expected_ap = core_count.saturating_sub(1);
    serial_println_core!(
        "bsp_init: waiting for {} AP cores to be ready...",
        expected_ap
    );
    while AP_CORES_READY.load(Ordering::Acquire) < expected_ap {
        hlt();
    }
    serial_println_core!("bsp_init: all {} AP cores ready", expected_ap);
}

pub fn wait_for_ap_cores_with_timeout(timeout_ms: u64) -> bool {
    let core_count = CORE_POOL.lock().total_cores();
    let expected_ap = core_count.saturating_sub(1) as u64;
    if expected_ap == 0 {
        return true;
    }
    let start = crate::interrupts::system_uptime_ns();
    let timeout_ns = timeout_ms * 1_000_000;
    loop {
        let ready = AP_CORES_READY.load(Ordering::Acquire) as u64;
        if ready >= expected_ap {
            serial_println_core!("bsp_init: all {} AP cores ready", expected_ap);
            return true;
        }
        let now = crate::interrupts::system_uptime_ns();
        if now.saturating_sub(start) >= timeout_ns {
            serial_println_core!(
                "bsp_init: timeout waiting for AP cores: ready {}/{} after {} ms",
                ready,
                expected_ap,
                timeout_ms
            );
            return false;
        }
        hlt();
    }
}

pub fn expected_ap_cores() -> u8 {
    let core_count = CORE_POOL.lock().total_cores();
    core_count.saturating_sub(1)
}

pub fn check_ap_cores_ready() -> (u8, u8) {
    let expected = expected_ap_cores();
    let ready = AP_CORES_READY.load(Ordering::Acquire);
    (ready, expected)
}

pub fn verify_smp_boot(timeout_ms: u64) -> bool {
    let ok = wait_for_ap_cores_with_timeout(timeout_ms);
    let (ready, expected) = check_ap_cores_ready();
    serial_println_core!(
        "verify_smp_boot: ready={} expected={} timeout={}ms result={}",
        ready,
        expected,
        timeout_ms,
        ok
    );
    if !ok {
        return false;
    }
    if ready != expected {
        serial_println_core!(
            "verify_smp_boot: mismatch ready {} != expected {}",
            ready,
            expected
        );
        return false;
    }
    let total = CORE_POOL.lock().total_cores();
    if total == 0 || total > MAX_CORES {
        serial_println_core!(
            "verify_smp_boot: total_cores {} out of range 1..={}",
            total,
            MAX_CORES
        );
        return false;
    }
    if total as u64 != (expected as u64 + 1) {
        serial_println_core!(
            "verify_smp_boot: total {} != expected+1 {}",
            total,
            expected + 1
        );
        return false;
    }
    true
}
