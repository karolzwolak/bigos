#![no_std]
#![no_main]

//! PID 1, started by the kernel before any other userspace process
//! It adopts the children of processes that exit with ChildPolicy::OrphanChildren

use core::sync::atomic::AtomicU32;
use rustspace::{sys_exit, sys_futex_wait};

const PARK_VALUE: u32 = 0;
const EXIT_CODE_PANIC: i32 = 255;

/// Never changes, so the futex wait below never returns
static PARK_WORD: AtomicU32 = AtomicU32::new(PARK_VALUE);

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
        "call {bigproc_main}",
        "ud2",
        bigproc_main = sym bigproc_main,
    )
}

extern "C" fn bigproc_main() -> ! {
    loop {
        unsafe { sys_futex_wait(PARK_WORD.as_ptr(), PARK_VALUE) };
    }
}
