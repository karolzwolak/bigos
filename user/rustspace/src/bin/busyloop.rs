#![no_std]
#![no_main]

use core::sync::atomic::Ordering;
use rustspace::{program_shared_data, sys_exit};

// Indexes into the shared test counters, the kernel tests use the same values
const COUNTER_RUNNING: usize = 0;
const COUNTER_STOP: usize = 1;

const EXIT_CODE_PANIC: i32 = 255;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { sys_exit(EXIT_CODE_PANIC) }
}

/// Announces itself, then occupies the cpu without ever entering the kernel until told to stop
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let counters = &program_shared_data().test_counters;
    counters[COUNTER_RUNNING].fetch_add(1, Ordering::AcqRel);

    while counters[COUNTER_STOP].load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }

    unsafe { sys_exit(0) }
}
