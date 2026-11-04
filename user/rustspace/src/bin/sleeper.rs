#![no_std]
#![no_main]

use core::arch::x86_64::_rdtsc;
use core::sync::atomic::Ordering;
use rustspace::{CpuInfoFlat, program_shared_data, sys_exit, sys_get_cpu_info, sys_sleep_ms};

// Indexes into the shared test counters, the kernel tests use the same values
const COUNTER_RUNNING: usize = 0;
const COUNTER_SLEEP_MS: usize = 4;
const COUNTER_SLEEPING_TASKS_DONE: usize = 5;

const MICROSECONDS_PER_MILLISECOND: u64 = 1000;
const MICROSECONDS_PER_SECOND: u64 = 1_000_000;

const EXIT_CODE_PANIC: i32 = 255;
const EXIT_CODE_NO_CPU_INFO: i32 = 2;
const EXIT_CODE_WOKE_TOO_EARLY: i32 = 3;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { sys_exit(EXIT_CODE_PANIC) }
}

// the kernel enters with a 16 byte aligned rsp, the ABI wants it 8 off at function entry
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {sleep_main}",
        "ud2",
        sleep_main = sym sleep_main,
    )
}

/// Sleeps for the time the kernel test asked for and checks with its own clock that
/// the sleep was not shorter
extern "C" fn sleep_main() -> ! {
    let counters = &program_shared_data().test_counters;
    let sleep_ms = counters[COUNTER_SLEEP_MS].load(Ordering::Acquire) as u64;

    let mut cpu_info = CpuInfoFlat::zeroed();
    if !unsafe { sys_get_cpu_info(&mut cpu_info) } || cpu_info.tsc_frequency_hz == 0 {
        unsafe { sys_exit(EXIT_CODE_NO_CPU_INFO) }
    }

    counters[COUNTER_RUNNING].fetch_add(1, Ordering::AcqRel);

    // rdtscp is not offered by the cpu model the tests boot with
    let start_tsc = unsafe { _rdtsc() };
    unsafe { sys_sleep_ms(sleep_ms) };
    let end_tsc = unsafe { _rdtsc() };

    let elapsed_us = (end_tsc - start_tsc) as u128 * MICROSECONDS_PER_SECOND as u128
        / cpu_info.tsc_frequency_hz as u128;
    if elapsed_us < (sleep_ms * MICROSECONDS_PER_MILLISECOND) as u128 {
        unsafe { sys_exit(EXIT_CODE_WOKE_TOO_EARLY) }
    }

    counters[COUNTER_SLEEPING_TASKS_DONE].fetch_add(1, Ordering::AcqRel);

    unsafe { sys_exit(0) }
}
