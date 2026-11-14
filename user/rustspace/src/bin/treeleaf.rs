#![no_std]
#![no_main]

use core::sync::atomic::Ordering;
use rustspace::{program_shared_data, sys_exit, sys_sleep_ms};

// Indexes into the shared test counters, test_user_proctree and treeroot use the same values
// They overlap fs_test and spawner, which never run in the same test kernel
const COUNTER_LEAVES_STARTED: usize = 3;
const COUNTER_LEAF_RELEASE: usize = 4;

const POLL_INTERVAL_MS: u64 = 1;
const EXIT_CODE_PANIC: i32 = 255;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys_exit(EXIT_CODE_PANIC)
}

// the kernel enters with a 16 byte aligned rsp, the ABI wants it 8 off at function entry
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {treeleaf_main}",
        "ud2",
        treeleaf_main = sym treeleaf_main,
    )
}

/// Lives until the kernel test releases it or something kills it
extern "C" fn treeleaf_main() -> ! {
    let counters = &program_shared_data().test_counters;
    counters[COUNTER_LEAVES_STARTED].fetch_add(1, Ordering::AcqRel);

    while counters[COUNTER_LEAF_RELEASE].load(Ordering::Acquire) == 0 {
        sys_sleep_ms(POLL_INTERVAL_MS);
    }

    sys_exit(0)
}
