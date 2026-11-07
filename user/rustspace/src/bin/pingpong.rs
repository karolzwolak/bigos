#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};
use rustspace::{program_shared_data, sys_exit, sys_futex_wait, sys_futex_wake};

// Indexes into the shared test counters, the kernel tests use the same values
const COUNTER_PING_TICKET: usize = 6;
const COUNTER_PING_TURN: usize = 7;
const COUNTER_PING_ROUNDS_BASE: usize = 8;

const ROLE_COUNT: u32 = 2;
const ROUNDS: u32 = 2000;

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
        "call {ping_main}",
        "ud2",
        ping_main = sym ping_main,
    )
}

fn wait_for_turn(turn: &AtomicU32, role: u32) {
    let other_role = ROLE_COUNT - 1 - role;

    // a wake that lands between the load and the wait must not be lost, the kernel
    // rechecks the word after it queued the task
    while turn.load(Ordering::Acquire) != role {
        unsafe { sys_futex_wait(turn.as_ptr(), other_role) };
    }
}

/// Two processes hand a turn back and forth through a word they block on, so every
/// round is a block on one core racing with a wake from another
extern "C" fn ping_main() -> ! {
    let counters = &program_shared_data().test_counters;
    let role = counters[COUNTER_PING_TICKET].fetch_add(1, Ordering::AcqRel);
    let turn = &counters[COUNTER_PING_TURN];

    for _ in 0..ROUNDS {
        wait_for_turn(turn, role);

        counters[COUNTER_PING_ROUNDS_BASE + role as usize].fetch_add(1, Ordering::AcqRel);

        turn.store(ROLE_COUNT - 1 - role, Ordering::Release);
        unsafe { sys_futex_wake() };
    }

    unsafe { sys_exit(0) }
}
