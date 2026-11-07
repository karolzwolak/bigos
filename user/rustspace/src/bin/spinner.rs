#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::Ordering;
use rustspace::{program_shared_data, sys_exit};

// Indexes into the shared test counters, the kernel test uses the same values
const COUNTER_TICKET: usize = 0;
const COUNTER_SPINNER_COUNT: usize = 1;
const COUNTER_PROGRESS_BASE: usize = 2;

const BURN_ITERATIONS: u64 = 250_000_000;
const ROUNDS: u32 = 3;

const EXIT_CODE_PANIC: i32 = 255;
const EXIT_CODE_REGISTERS_CORRUPTED: i32 = 2;

const SEED_MULTIPLIER: u64 = 0x9E37_79B9_7F4A_7C15;
const GPR_PROBE_COUNT: usize = 8;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { sys_exit(EXIT_CODE_PANIC) }
}

/// Spins without ever calling the kernel while r8-r15 and xmm0-xmm1 hold values that
/// are different for every spinner and round, returns whether they survived
/// The time slice ends in the middle of this loop, so a preemption that loses a
/// register shows up here
#[inline(never)]
fn burn_with_registers(seed: u64) -> bool {
    let mut expected_gprs = [0u64; GPR_PROBE_COUNT];
    for (index, value) in expected_gprs.iter_mut().enumerate() {
        *value = seed
            .wrapping_mul(SEED_MULTIPLIER)
            .wrapping_add(index as u64);
    }
    let expected_xmm = [!seed, seed.rotate_left(17)];

    let [g0, g1, g2, g3, g4, g5, g6, g7] = expected_gprs;
    let (o0, o1, o2, o3, o4, o5, o6, o7): (u64, u64, u64, u64, u64, u64, u64, u64);
    let (x0_out, x1_out): (u64, u64);

    unsafe {
        asm!(
            "movq xmm0, {x0}",
            "movq xmm1, {x1}",
            "2:",
            "dec {count}",
            "jnz 2b",
            "movq {x0}, xmm0",
            "movq {x1}, xmm1",
            count = inout(reg) BURN_ITERATIONS => _,
            x0 = inout(reg) expected_xmm[0] => x0_out,
            x1 = inout(reg) expected_xmm[1] => x1_out,
            inout("r8") g0 => o0,
            inout("r9") g1 => o1,
            inout("r10") g2 => o2,
            inout("r11") g3 => o3,
            inout("r12") g4 => o4,
            inout("r13") g5 => o5,
            inout("r14") g6 => o6,
            inout("r15") g7 => o7,
            out("xmm0") _,
            out("xmm1") _,
            options(nostack, nomem),
        );
    }

    let observed_gprs = [o0, o1, o2, o3, o4, o5, o6, o7];
    let observed_xmm = [x0_out, x1_out];

    observed_gprs == expected_gprs && observed_xmm == expected_xmm
}

// the kernel enters with a 16 byte aligned rsp, the ABI wants it 8 off at function entry
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {spin_main}",
        "ud2",
        spin_main = sym spin_main,
    )
}

extern "C" fn spin_main() -> ! {
    let counters = &program_shared_data().test_counters;
    let spinner_index = counters[COUNTER_TICKET].fetch_add(1, Ordering::AcqRel) as usize;
    let spinner_count = counters[COUNTER_SPINNER_COUNT].load(Ordering::Acquire) as usize;

    for round in 0..ROUNDS {
        let seed = ((spinner_index as u64) << 32) | round as u64;
        if !burn_with_registers(seed) {
            unsafe { sys_exit(EXIT_CODE_REGISTERS_CORRUPTED) }
        }

        counters[COUNTER_PROGRESS_BASE + spinner_index].store(round + 1, Ordering::Release);
    }

    // keep spinning until every other spinner has finished too, which only happens
    // when the scheduler takes the core away from spinners that never yield
    loop {
        let everyone_done = (0..spinner_count)
            .all(|index| counters[COUNTER_PROGRESS_BASE + index].load(Ordering::Acquire) >= ROUNDS);
        if everyone_done {
            break;
        }

        core::hint::spin_loop();
    }

    unsafe { sys_exit(0) }
}
