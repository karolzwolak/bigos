#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::Ordering;
use rustspace::{program_shared_data, sys_exit};

// Indexes into the shared test counters, the kernel tests use the same values
const COUNTER_WORKERS_DONE: usize = 3;

// takes several time slices of a low priority task
const WORK_ITERATIONS: u64 = 150_000_000;

const EXIT_CODE_PANIC: i32 = 255;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { sys_exit(EXIT_CODE_PANIC) }
}

/// Finite cpu work that never enters the kernel, then reports that it finished
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    unsafe {
        asm!(
            "2:",
            "dec {count}",
            "jnz 2b",
            count = inout(reg) WORK_ITERATIONS => _,
            options(nostack, nomem),
        );
    }

    program_shared_data().test_counters[COUNTER_WORKERS_DONE].fetch_add(1, Ordering::AcqRel);

    unsafe { sys_exit(0) }
}
