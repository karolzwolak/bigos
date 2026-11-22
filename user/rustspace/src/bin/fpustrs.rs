#![no_std]
#![no_main]

use core::arch::asm;
use core::arch::x86_64::_rdtsc;
use core::mem::offset_of;
use rustspace::{
    CpuInfoFlat, SYS_YIELD, sys_exit, sys_get_cpu_info, sys_get_handoff_count, sys_get_pid,
    sys_write, sys_yield,
};

const XMM_REGISTER_COUNT: usize = 16;

// reset values, every FP exception masked
const MXCSR_DEFAULT: u32 = 0x1F80;
const FCW_DEFAULT: u16 = 0x37F;

const MXCSR_ROUNDING_SHIFT: u32 = 13;
const MXCSR_FLUSH_TO_ZERO: u32 = 1 << 15;
const FCW_ROUNDING_SHIFT: u32 = 10;
const FCW_PRECISION_MASK: u16 = 0b11 << 8;
const FCW_PRECISION_DOUBLE: u16 = 0b10 << 8;
const ROUNDING_MODE_MASK: usize = 0b11;

const MS_PER_SECOND: u64 = 1000;
/// Longer than a scheduler slice, so a waiting thread preempts the spin
const SPIN_DURATION_MS: u64 = 150;
const HANDOFF_TIMEOUT_MS: u64 = 5000;
/// Keeps the core busy once the probes are done, an idle core would steal the partner
/// of a thread that still probes and leave it alone on its core
const LINGER_MS: u64 = 3000;
/// Probes that must see another thread run on the core while the state is loaded
const REQUIRED_HANDOFFS: u32 = 3;

const EXIT_CODE_PANIC: i32 = 1;
const EXIT_CODE_DIRTY_INITIAL_XMM: i32 = 2;
const EXIT_CODE_BAD_INITIAL_MXCSR: i32 = 3;
const EXIT_CODE_FPU_LOST_ACROSS_YIELD: i32 = 4;
const EXIT_CODE_BAD_INITIAL_FCW: i32 = 5;
const EXIT_CODE_FPU_LOST_ACROSS_PREEMPTION: i32 = 6;
const EXIT_CODE_NO_HANDOFF: i32 = 7;

/// xmm0-15 plus the SSE and x87 control words, the layout the asm macros store
#[repr(C, align(16))]
#[derive(Clone, Copy, PartialEq)]
struct FpuBank {
    xmm: [u128; XMM_REGISTER_COUNT],
    mxcsr: u32,
    fcw: u16,
}

impl FpuBank {
    const fn zeroed() -> Self {
        Self {
            xmm: [0; XMM_REGISTER_COUNT],
            mxcsr: 0,
            fcw: 0,
        }
    }

    /// Unique per process and round, the register index sits in the low bits so a
    /// swapped or stale register shows
    fn for_round(process_seed: u128, round: u32, mxcsr: u32, fcw: u16) -> Self {
        let mut bank = Self {
            xmm: [0; XMM_REGISTER_COUNT],
            mxcsr,
            fcw,
        };

        for (register_index, value) in bank.xmm.iter_mut().enumerate() {
            *value = (process_seed << 64) | ((round as u128) << 32) | register_index as u128;
        }

        bank
    }
}

const MXCSR_OFFSET: usize = offset_of!(FpuBank, mxcsr);
const FCW_OFFSET: usize = offset_of!(FpuBank, fcw);

/// What a new thread must start with, the asm also loads its control words back after a probe
static DEFAULT_FPU_BANK: FpuBank = FpuBank {
    xmm: [0; XMM_REGISTER_COUNT],
    mxcsr: MXCSR_DEFAULT,
    fcw: FCW_DEFAULT,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum PhaseResult {
    Passed,
    StateLost,
    NoHandoff,
}

#[derive(Clone, Copy)]
enum Phase {
    Yield,
    Preemption,
}

// every message goes out in one write, so the lines of parallel instances do not interleave
impl Phase {
    fn state_lost_message(self) -> &'static [u8] {
        match self {
            Phase::Yield => b"fpustress: FAIL fpu state lost across yield\n",
            Phase::Preemption => b"fpustress: FAIL fpu state lost across preemption\n",
        }
    }

    fn no_handoff_message(self) -> &'static [u8] {
        match self {
            Phase::Yield => b"fpustress: FAIL no other thread ran during yield\n",
            Phase::Preemption => b"fpustress: FAIL no other thread ran during preemption\n",
        }
    }

    fn state_lost_exit_code(self) -> i32 {
        match self {
            Phase::Yield => EXIT_CODE_FPU_LOST_ACROSS_YIELD,
            Phase::Preemption => EXIT_CODE_FPU_LOST_ACROSS_PREEMPTION,
        }
    }
}

macro_rules! store_fpu_bank {
    ($base:literal) => {
        concat!(
            "movdqa [",
            $base,
            " + 0x00], xmm0\n",
            "movdqa [",
            $base,
            " + 0x10], xmm1\n",
            "movdqa [",
            $base,
            " + 0x20], xmm2\n",
            "movdqa [",
            $base,
            " + 0x30], xmm3\n",
            "movdqa [",
            $base,
            " + 0x40], xmm4\n",
            "movdqa [",
            $base,
            " + 0x50], xmm5\n",
            "movdqa [",
            $base,
            " + 0x60], xmm6\n",
            "movdqa [",
            $base,
            " + 0x70], xmm7\n",
            "movdqa [",
            $base,
            " + 0x80], xmm8\n",
            "movdqa [",
            $base,
            " + 0x90], xmm9\n",
            "movdqa [",
            $base,
            " + 0xa0], xmm10\n",
            "movdqa [",
            $base,
            " + 0xb0], xmm11\n",
            "movdqa [",
            $base,
            " + 0xc0], xmm12\n",
            "movdqa [",
            $base,
            " + 0xd0], xmm13\n",
            "movdqa [",
            $base,
            " + 0xe0], xmm14\n",
            "movdqa [",
            $base,
            " + 0xf0], xmm15\n",
            "stmxcsr [",
            $base,
            " + {mxcsr_offset}]\n",
            "fnstcw [",
            $base,
            " + {fcw_offset}]\n",
        )
    };
}

macro_rules! load_fpu_bank {
    ($base:literal) => {
        concat!(
            "movdqa xmm0, [",
            $base,
            " + 0x00]\n",
            "movdqa xmm1, [",
            $base,
            " + 0x10]\n",
            "movdqa xmm2, [",
            $base,
            " + 0x20]\n",
            "movdqa xmm3, [",
            $base,
            " + 0x30]\n",
            "movdqa xmm4, [",
            $base,
            " + 0x40]\n",
            "movdqa xmm5, [",
            $base,
            " + 0x50]\n",
            "movdqa xmm6, [",
            $base,
            " + 0x60]\n",
            "movdqa xmm7, [",
            $base,
            " + 0x70]\n",
            "movdqa xmm8, [",
            $base,
            " + 0x80]\n",
            "movdqa xmm9, [",
            $base,
            " + 0x90]\n",
            "movdqa xmm10, [",
            $base,
            " + 0xa0]\n",
            "movdqa xmm11, [",
            $base,
            " + 0xb0]\n",
            "movdqa xmm12, [",
            $base,
            " + 0xc0]\n",
            "movdqa xmm13, [",
            $base,
            " + 0xd0]\n",
            "movdqa xmm14, [",
            $base,
            " + 0xe0]\n",
            "movdqa xmm15, [",
            $base,
            " + 0xf0]\n",
            "ldmxcsr [",
            $base,
            " + {mxcsr_offset}]\n",
            "fldcw [",
            $base,
            " + {fcw_offset}]\n",
        )
    };
}

// Rust code assumes the default rounding and precision, so no probe leaves its own behind
macro_rules! load_default_control_words {
    () => {
        concat!(
            "ldmxcsr [rip + {defaults} + {mxcsr_offset}]\n",
            "fldcw [rip + {defaults} + {fcw_offset}]\n",
        )
    };
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys_exit(EXIT_CODE_PANIC)
}

fn write_str(s: &[u8]) {
    unsafe { sys_write(1, s.as_ptr(), s.len()) };
}

fn read_tsc() -> u64 {
    unsafe { _rdtsc() }
}

fn tsc_ticks_per_ms() -> u64 {
    let mut cpu_info = CpuInfoFlat::zeroed();
    let info_written = sys_get_cpu_info(&mut cpu_info);
    debug_assert!(info_written && cpu_info.tsc_frequency_hz >= MS_PER_SECOND);

    cpu_info.tsc_frequency_hz / MS_PER_SECOND
}

#[inline(never)]
unsafe fn yield_with_loaded_fpu(expected_bank: *const FpuBank, actual_bank: *mut FpuBank) {
    unsafe {
        asm!(
            "push rbx",
            "push rbp",
            "push r12",
            "push r13",
            "push r14",
            "push r15",
            "push rdi",
            load_fpu_bank!("rsi"),
            "syscall",
            "pop rdi",
            store_fpu_bank!("rdi"),
            load_default_control_words!(),
            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop rbp",
            "pop rbx",
            mxcsr_offset = const MXCSR_OFFSET,
            fcw_offset = const FCW_OFFSET,
            defaults = sym DEFAULT_FPU_BANK,
            inout("rax") SYS_YIELD => _,
            inout("rdi") actual_bank => _,
            inout("rsi") expected_bank => _,
            out("rcx") _,
            out("rdx") _,
            out("r8") _,
            out("r9") _,
            out("r10") _,
            out("r11") _,
            out("xmm0") _,
            out("xmm1") _,
            out("xmm2") _,
            out("xmm3") _,
            out("xmm4") _,
            out("xmm5") _,
            out("xmm6") _,
            out("xmm7") _,
            out("xmm8") _,
            out("xmm9") _,
            out("xmm10") _,
            out("xmm11") _,
            out("xmm12") _,
            out("xmm13") _,
            out("xmm14") _,
            out("xmm15") _,
        );
    }
}

/// Holds the bank in the registers without entering the kernel until `deadline_tsc`,
/// so only a timer interrupt can switch the thread away
#[inline(never)]
unsafe fn spin_with_loaded_fpu(
    expected_bank: *const FpuBank,
    actual_bank: *mut FpuBank,
    deadline_tsc: u64,
) {
    unsafe {
        asm!(
            load_fpu_bank!("rsi"),
            "2:",
            "rdtsc",
            "shl rdx, 32",
            "or rax, rdx",
            "cmp rax, r8",
            "jb 2b",
            store_fpu_bank!("rdi"),
            load_default_control_words!(),
            mxcsr_offset = const MXCSR_OFFSET,
            fcw_offset = const FCW_OFFSET,
            defaults = sym DEFAULT_FPU_BANK,
            in("rdi") actual_bank,
            in("rsi") expected_bank,
            in("r8") deadline_tsc,
            out("rax") _,
            out("rdx") _,
            out("xmm0") _,
            out("xmm1") _,
            out("xmm2") _,
            out("xmm3") _,
            out("xmm4") _,
            out("xmm5") _,
            out("xmm6") _,
            out("xmm7") _,
            out("xmm8") _,
            out("xmm9") _,
            out("xmm10") _,
            out("xmm11") _,
            out("xmm12") _,
            out("xmm13") _,
            out("xmm14") _,
            out("xmm15") _,
        );
    }
}

/// Repeats `probe` until REQUIRED_HANDOFFS of its runs saw another thread take the core,
/// `probe` returns false when the state it loaded did not come back
fn run_phase_across_handoffs(timeout_tsc: u64, mut probe: impl FnMut(u32) -> bool) -> PhaseResult {
    let deadline_tsc = read_tsc() + timeout_tsc;
    let mut probes_with_handoff = 0;
    let mut round = 0;

    while probes_with_handoff < REQUIRED_HANDOFFS {
        if read_tsc() >= deadline_tsc {
            return PhaseResult::NoHandoff;
        }

        let handoffs_before = sys_get_handoff_count();
        if !probe(round) {
            return PhaseResult::StateLost;
        }

        if sys_get_handoff_count() != handoffs_before {
            probes_with_handoff += 1;
        }
        round += 1;
    }

    PhaseResult::Passed
}

fn exit_on_failed_phase(phase_result: PhaseResult, phase: Phase) {
    match phase_result {
        PhaseResult::Passed => {}
        PhaseResult::StateLost => {
            write_str(phase.state_lost_message());
            sys_exit(phase.state_lost_exit_code())
        }
        PhaseResult::NoHandoff => {
            write_str(phase.no_handoff_message());
            sys_exit(EXIT_CODE_NO_HANDOFF)
        }
    }
}

/// Written by _start before any compiled code can touch the fpu
static mut INITIAL_FPU_BANK: FpuBank = FpuBank::zeroed();

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "lea rdi, [rip + {initial_bank}]",
        store_fpu_bank!("rdi"),
        "and rsp, -16",
        "call {start_checked}",
        "ud2",
        initial_bank = sym INITIAL_FPU_BANK,
        mxcsr_offset = const MXCSR_OFFSET,
        fcw_offset = const FCW_OFFSET,
        start_checked = sym start_checked,
    )
}

extern "C" fn start_checked() -> ! {
    let initial_bank = unsafe { core::ptr::addr_of!(INITIAL_FPU_BANK).read() };

    if initial_bank.xmm != DEFAULT_FPU_BANK.xmm {
        write_str(b"fpustress: FAIL initial xmm not zero\n");
        sys_exit(EXIT_CODE_DIRTY_INITIAL_XMM)
    }
    if initial_bank.mxcsr != MXCSR_DEFAULT {
        write_str(b"fpustress: FAIL initial mxcsr\n");
        sys_exit(EXIT_CODE_BAD_INITIAL_MXCSR)
    }
    if initial_bank.fcw != FCW_DEFAULT {
        write_str(b"fpustress: FAIL initial fcw\n");
        sys_exit(EXIT_CODE_BAD_INITIAL_FCW)
    }

    let start_tsc = read_tsc();
    let ticks_per_ms = tsc_ticks_per_ms();
    let process_seed = start_tsc as u128;

    // partners on a core get consecutive pids most of the time, so their modes differ
    let rounding_mode = sys_get_pid() & ROUNDING_MODE_MASK;
    let probe_mxcsr =
        MXCSR_DEFAULT | MXCSR_FLUSH_TO_ZERO | ((rounding_mode as u32) << MXCSR_ROUNDING_SHIFT);
    let probe_fcw = (FCW_DEFAULT & !FCW_PRECISION_MASK)
        | FCW_PRECISION_DOUBLE
        | ((rounding_mode as u16) << FCW_ROUNDING_SHIFT);
    debug_assert!(probe_mxcsr != MXCSR_DEFAULT && probe_fcw != FCW_DEFAULT);

    let yield_result = run_phase_across_handoffs(HANDOFF_TIMEOUT_MS * ticks_per_ms, |round| {
        let expected_bank = FpuBank::for_round(process_seed, round, probe_mxcsr, probe_fcw);
        let mut actual_bank = FpuBank::zeroed();
        unsafe { yield_with_loaded_fpu(&expected_bank, &mut actual_bank) };

        actual_bank == expected_bank
    });
    exit_on_failed_phase(yield_result, Phase::Yield);

    let preemption_result = run_phase_across_handoffs(HANDOFF_TIMEOUT_MS * ticks_per_ms, |round| {
        let expected_bank = FpuBank::for_round(process_seed, round, probe_mxcsr, probe_fcw);
        let mut actual_bank = FpuBank::zeroed();
        let deadline_tsc = read_tsc() + SPIN_DURATION_MS * ticks_per_ms;
        unsafe { spin_with_loaded_fpu(&expected_bank, &mut actual_bank, deadline_tsc) };

        actual_bank == expected_bank
    });
    exit_on_failed_phase(preemption_result, Phase::Preemption);

    let linger_end_tsc = start_tsc + LINGER_MS * ticks_per_ms;
    while read_tsc() < linger_end_tsc {
        sys_yield();
    }

    write_str(b"fpustress: ok\n");
    sys_exit(0)
}
