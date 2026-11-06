#![no_std]
#![no_main]

use core::sync::atomic::Ordering;
use rustspace::{program_shared_data, sys_exit, sys_yield};

// Indexes into the shared test counters, the kernel tests use the same values
const COUNTER_RUNNING: usize = 0;
const COUNTER_STOP: usize = 1;
const COUNTER_YIELDS: usize = 2;

const EXIT_CODE_PANIC: i32 = 255;

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
        "call {yield_main}",
        "ud2",
        yield_main = sym yield_main,
    )
}

/// Yields over and over until told to stop, counting how often it got the cpu back
extern "C" fn yield_main() -> ! {
    let counters = &program_shared_data().test_counters;
    counters[COUNTER_RUNNING].fetch_add(1, Ordering::AcqRel);

    while counters[COUNTER_STOP].load(Ordering::Acquire) == 0 {
        unsafe { sys_yield() };
        counters[COUNTER_YIELDS].fetch_add(1, Ordering::AcqRel);
    }

    unsafe { sys_exit(0) }
}
