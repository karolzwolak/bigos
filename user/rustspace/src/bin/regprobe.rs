#![no_std]
#![no_main]

use core::arch::asm;
use core::arch::x86_64::_rdtsc;
use core::mem::{offset_of, size_of};
use rustspace::{
    CpuInfoFlat, SYS_GET_PID, SYS_YIELD, sys_exit, sys_get_cpu_info, sys_get_handoff_count,
    sys_get_pid, sys_write, sys_yield,
};

const SYSCALL_PROBE_ROUNDS: u32 = 16;
const HEX_DIGIT_COUNT: usize = 16;
const BITS_PER_HEX_DIGIT: usize = 4;
const DECIMAL_BASE: u8 = 10;
const LINE_CAPACITY: usize = 96;

const MS_PER_SECOND: u64 = 1000;
/// Longer than a scheduler slice, so a waiting task preempts the spin
const SPIN_DURATION_MS: u64 = 150;
const HANDOFF_TIMEOUT_MS: u64 = 5000;
/// Keeps the core busy once the probes are done, an idle core would steal the partner
/// of a task that still probes and leave it alone on its core
const LINGER_MS: u64 = 3000;
/// Probes that must see another task run on the core while the registers are loaded
const REQUIRED_HANDOFFS: u32 = 3;
const CALIBRATION_SPIN_ITERATIONS: u64 = 1 << 16;
const CALIBRATION_RUNS: u32 = 4;

const PID_SEED_SHIFT: u32 = 48;
// odd, so every register index lands on a different value
const REGISTER_SEED_STRIDE: u64 = 0x9E37_79B9_7F4A_7C15;

const RFLAGS_INTERRUPT: u64 = 1 << 9;
const RFLAGS_DIRECTION: u64 = 1 << 10;
const RFLAGS_ALIGNMENT_CHECK: u64 = 1 << 18;
/// Set around every probe, the kernel has to clear them for itself and give them back
const PROBE_RFLAGS: u64 = RFLAGS_DIRECTION | RFLAGS_ALIGNMENT_CHECK;
const CHECKED_RFLAGS: u64 = PROBE_RFLAGS | RFLAGS_INTERRUPT;

const EXIT_CODE_PANIC: i32 = 255;
const EXIT_CODE_LOST_REGISTER_ON_SYSRET: i32 = 100;
const EXIT_CODE_LOST_REGISTER_ON_YIELD: i32 = 101;
const EXIT_CODE_YIELD_RETURNED_NONZERO: i32 = 102;
const EXIT_CODE_LOST_REGISTER_ON_PREEMPTION: i32 = 103;
const EXIT_CODE_GET_PID_RETURNED_WRONG: i32 = 104;
const EXIT_CODE_NO_HANDOFF: i32 = 105;

#[derive(Clone, Copy)]
#[repr(usize)]
enum Gpr {
    Rax,
    Rbx,
    Rcx,
    Rdx,
    Rbp,
    Rsi,
    Rdi,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
}

const GPR_COUNT: usize = Gpr::R15 as usize + 1;

const GPR_NAMES: [&[u8]; GPR_COUNT] = [
    b"rax", b"rbx", b"rcx", b"rdx", b"rbp", b"rsi", b"rdi", b"r8", b"r9", b"r10", b"r11", b"r12",
    b"r13", b"r14", b"r15",
];

const fn gpr_bit(gpr: Gpr) -> u32 {
    1 << gpr as u32
}

const ALL_GPRS: u32 = (1 << GPR_COUNT) - 1;
/// rax carries the result, rcx and r11 are clobbered by the syscall instruction itself
const SYSCALL_PRESERVED_GPRS: u32 =
    ALL_GPRS & !(gpr_bit(Gpr::Rax) | gpr_bit(Gpr::Rcx) | gpr_bit(Gpr::R11));

#[repr(C)]
#[derive(Clone, Copy)]
struct RegisterSet {
    gprs: [u64; GPR_COUNT],
    rsp: u64,
    rflags: u64,
}

impl RegisterSet {
    const fn zeroed() -> Self {
        Self {
            gprs: [0; GPR_COUNT],
            rsp: 0,
            rflags: 0,
        }
    }
}

const fn gpr_offset(gpr: Gpr) -> usize {
    offset_of!(RegisterSet, gprs) + gpr as usize * size_of::<u64>()
}

/// Loaded into the registers by a probe, rsp is recorded by the probe itself
static mut EXPECTED: RegisterSet = RegisterSet::zeroed();
/// What the registers held after the probe
static mut OBSERVED: RegisterSet = RegisterSet::zeroed();
static mut SPIN_ITERATIONS_LEFT: u64 = 0;

/// Loads EXPECTED into every general register, sets PROBE_RFLAGS, runs `$action` and
/// stores every register into OBSERVED. rbx and rbp cannot be asm operands, so the
/// callee-saved registers are saved by hand
macro_rules! probe_with_loaded_registers {
    ($action:literal, $($extra_operands:tt)*) => {
        asm!(
            "push rbx",
            "push rbp",
            "push r12",
            "push r13",
            "push r14",
            "push r15",
            "pushfq",
            "pushfq",
            "or qword ptr [rsp], {probe_rflags}",
            "popfq",

            "mov rbx, [rip + {expected} + {rbx_offset}]",
            "mov rcx, [rip + {expected} + {rcx_offset}]",
            "mov rdx, [rip + {expected} + {rdx_offset}]",
            "mov rbp, [rip + {expected} + {rbp_offset}]",
            "mov rsi, [rip + {expected} + {rsi_offset}]",
            "mov rdi, [rip + {expected} + {rdi_offset}]",
            "mov r8, [rip + {expected} + {r8_offset}]",
            "mov r9, [rip + {expected} + {r9_offset}]",
            "mov r10, [rip + {expected} + {r10_offset}]",
            "mov r11, [rip + {expected} + {r11_offset}]",
            "mov r12, [rip + {expected} + {r12_offset}]",
            "mov r13, [rip + {expected} + {r13_offset}]",
            "mov r14, [rip + {expected} + {r14_offset}]",
            "mov r15, [rip + {expected} + {r15_offset}]",
            "mov [rip + {expected} + {rsp_offset}], rsp",
            "mov rax, [rip + {expected} + {rax_offset}]",

            $action,

            "mov [rip + {observed} + {rax_offset}], rax",
            "mov [rip + {observed} + {rbx_offset}], rbx",
            "mov [rip + {observed} + {rcx_offset}], rcx",
            "mov [rip + {observed} + {rdx_offset}], rdx",
            "mov [rip + {observed} + {rbp_offset}], rbp",
            "mov [rip + {observed} + {rsi_offset}], rsi",
            "mov [rip + {observed} + {rdi_offset}], rdi",
            "mov [rip + {observed} + {r8_offset}], r8",
            "mov [rip + {observed} + {r9_offset}], r9",
            "mov [rip + {observed} + {r10_offset}], r10",
            "mov [rip + {observed} + {r11_offset}], r11",
            "mov [rip + {observed} + {r12_offset}], r12",
            "mov [rip + {observed} + {r13_offset}], r13",
            "mov [rip + {observed} + {r14_offset}], r14",
            "mov [rip + {observed} + {r15_offset}], r15",
            "mov [rip + {observed} + {rsp_offset}], rsp",
            "pushfq",
            "pop qword ptr [rip + {observed} + {rflags_offset}]",

            "popfq",
            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop rbp",
            "pop rbx",

            $($extra_operands)*
            expected = sym EXPECTED,
            observed = sym OBSERVED,
            probe_rflags = const PROBE_RFLAGS,
            rax_offset = const gpr_offset(Gpr::Rax),
            rbx_offset = const gpr_offset(Gpr::Rbx),
            rcx_offset = const gpr_offset(Gpr::Rcx),
            rdx_offset = const gpr_offset(Gpr::Rdx),
            rbp_offset = const gpr_offset(Gpr::Rbp),
            rsi_offset = const gpr_offset(Gpr::Rsi),
            rdi_offset = const gpr_offset(Gpr::Rdi),
            r8_offset = const gpr_offset(Gpr::R8),
            r9_offset = const gpr_offset(Gpr::R9),
            r10_offset = const gpr_offset(Gpr::R10),
            r11_offset = const gpr_offset(Gpr::R11),
            r12_offset = const gpr_offset(Gpr::R12),
            r13_offset = const gpr_offset(Gpr::R13),
            r14_offset = const gpr_offset(Gpr::R14),
            r15_offset = const gpr_offset(Gpr::R15),
            rsp_offset = const offset_of!(RegisterSet, rsp),
            rflags_offset = const offset_of!(RegisterSet, rflags),
            out("rax") _,
            out("rcx") _,
            out("rdx") _,
            out("rsi") _,
            out("rdi") _,
            out("r8") _,
            out("r9") _,
            out("r10") _,
            out("r11") _,
        )
    };
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys_exit(EXIT_CODE_PANIC)
}

#[inline(never)]
unsafe fn probe_syscall() {
    unsafe { probe_with_loaded_registers!("syscall",) };
}

/// Spins in ring 3 without touching a register until SPIN_ITERATIONS_LEFT runs out,
/// so only an interrupt can take the task away
#[inline(never)]
unsafe fn probe_spin() {
    unsafe {
        probe_with_loaded_registers!(
            "2: dec qword ptr [rip + {spin_left}]\njnz 2b",
            spin_left = sym SPIN_ITERATIONS_LEFT,
        )
    };
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

fn write_bytes(bytes: &[u8]) {
    unsafe { sys_write(1, bytes.as_ptr(), bytes.len()) };
}

/// Collects one output line, a single write keeps the lines of parallel instances apart
struct LineBuffer {
    bytes: [u8; LINE_CAPACITY],
    len: usize,
}

impl LineBuffer {
    const fn new() -> Self {
        Self {
            bytes: [0; LINE_CAPACITY],
            len: 0,
        }
    }

    fn push(&mut self, part: &[u8]) {
        debug_assert!(self.len + part.len() <= LINE_CAPACITY);

        self.bytes[self.len..self.len + part.len()].copy_from_slice(part);
        self.len += part.len();
    }

    fn push_hex(&mut self, value: u64) {
        let mut digits = [0u8; HEX_DIGIT_COUNT];

        for (index, digit) in digits.iter_mut().enumerate() {
            let shift = (HEX_DIGIT_COUNT - 1 - index) * BITS_PER_HEX_DIGIT;
            let nibble = ((value >> shift) & 0xF) as u8;
            *digit = if nibble < DECIMAL_BASE {
                b'0' + nibble
            } else {
                b'a' + nibble - DECIMAL_BASE
            };
        }

        self.push(&digits);
    }

    fn write(&self) {
        write_bytes(&self.bytes[..self.len]);
    }
}

fn report(label: &[u8], what: &[u8], value: u64) {
    let mut line = LineBuffer::new();
    line.push(b"regprobe: ");
    line.push(label);
    line.push(b" ");
    line.push(what);
    line.push(b"=");
    line.push_hex(value);
    line.push(b"\n");
    line.write();
}

/// Fresh values for every probe, so neither a stale frame nor the frame of another
/// regprobe instance matches them
fn prepare_expected(pid: u64, rax_value: u64) {
    let seed = read_tsc() ^ (pid << PID_SEED_SHIFT);
    let mut expected = RegisterSet::zeroed();

    for (gpr_index, value) in expected.gprs.iter_mut().enumerate() {
        *value = seed.wrapping_add(REGISTER_SEED_STRIDE.wrapping_mul(gpr_index as u64 + 1));
    }
    expected.gprs[Gpr::Rax as usize] = rax_value;

    unsafe { core::ptr::addr_of_mut!(EXPECTED).write(expected) };
}

/// Prints every register the probe did not get back, returns how many there were
fn count_lost_registers(label: &[u8], preserved_gprs: u32) -> u32 {
    let expected = unsafe { core::ptr::addr_of!(EXPECTED).read() };
    let observed = unsafe { core::ptr::addr_of!(OBSERVED).read() };
    let mut lost_count = 0;

    for gpr_index in 0..GPR_COUNT {
        if preserved_gprs & (1 << gpr_index) == 0
            || observed.gprs[gpr_index] == expected.gprs[gpr_index]
        {
            continue;
        }

        lost_count += 1;
        report(label, GPR_NAMES[gpr_index], observed.gprs[gpr_index]);
    }

    if observed.rsp != expected.rsp {
        lost_count += 1;
        report(label, b"rsp", observed.rsp);
    }

    if observed.rflags & CHECKED_RFLAGS != CHECKED_RFLAGS {
        lost_count += 1;
        report(label, b"rflags", observed.rflags);
    }

    lost_count
}

fn observed_rax() -> u64 {
    unsafe { core::ptr::addr_of!(OBSERVED).read() }.gprs[Gpr::Rax as usize]
}

fn probe_get_pid(pid: u64) {
    prepare_expected(pid, SYS_GET_PID);
    unsafe { probe_syscall() };

    if count_lost_registers(b"sysret lost", SYSCALL_PRESERVED_GPRS) != 0 {
        sys_exit(EXIT_CODE_LOST_REGISTER_ON_SYSRET)
    }

    let get_pid_result = observed_rax();
    if get_pid_result != pid {
        report(b"get_pid returned", b"rax", get_pid_result);
        sys_exit(EXIT_CODE_GET_PID_RETURNED_WRONG)
    }
}

fn probe_yield(pid: u64) {
    prepare_expected(pid, SYS_YIELD);
    unsafe { probe_syscall() };

    if count_lost_registers(b"yield lost", SYSCALL_PRESERVED_GPRS) != 0 {
        sys_exit(EXIT_CODE_LOST_REGISTER_ON_YIELD)
    }

    let yield_result = observed_rax();
    if yield_result != 0 {
        report(b"yield returned", b"rax", yield_result);
        sys_exit(EXIT_CODE_YIELD_RETURNED_NONZERO)
    }
}

fn probe_preemption(pid: u64, spin_iterations: u64) {
    debug_assert!(spin_iterations > 0);

    prepare_expected(pid, read_tsc());
    unsafe {
        core::ptr::addr_of_mut!(SPIN_ITERATIONS_LEFT).write(spin_iterations);
        probe_spin();
    }

    if count_lost_registers(b"preemption lost", ALL_GPRS) != 0 {
        sys_exit(EXIT_CODE_LOST_REGISTER_ON_PREEMPTION)
    }
}

/// Spin iterations that take SPIN_DURATION_MS, the fastest calibration run is the one
/// no interrupt stretched
fn calibrate_spin_iterations(pid: u64, ticks_per_ms: u64) -> u64 {
    let mut fastest_ticks = u64::MAX;

    for _ in 0..CALIBRATION_RUNS {
        let start_tsc = read_tsc();
        probe_preemption(pid, CALIBRATION_SPIN_ITERATIONS);
        fastest_ticks = fastest_ticks.min(read_tsc() - start_tsc);
    }

    CALIBRATION_SPIN_ITERATIONS * SPIN_DURATION_MS * ticks_per_ms / fastest_ticks.max(1)
}

/// Repeats `probe` until REQUIRED_HANDOFFS of its runs saw another task take the core
fn run_phase_across_handoffs(phase_name: &[u8], timeout_tsc: u64, mut probe: impl FnMut()) {
    let deadline_tsc = read_tsc() + timeout_tsc;
    let mut probes_with_handoff = 0;

    while probes_with_handoff < REQUIRED_HANDOFFS {
        if read_tsc() >= deadline_tsc {
            let mut line = LineBuffer::new();
            line.push(b"regprobe: no other task ran during ");
            line.push(phase_name);
            line.push(b"\n");
            line.write();
            sys_exit(EXIT_CODE_NO_HANDOFF)
        }

        let handoffs_before = sys_get_handoff_count();
        probe();
        if sys_get_handoff_count() != handoffs_before {
            probes_with_handoff += 1;
        }
    }
}

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {start_checked}",
        "ud2",
        start_checked = sym start_checked,
    )
}

extern "C" fn start_checked() -> ! {
    let start_tsc = read_tsc();
    let ticks_per_ms = tsc_ticks_per_ms();
    let pid = sys_get_pid() as u64;

    for _ in 0..SYSCALL_PROBE_ROUNDS {
        probe_get_pid(pid);
    }

    run_phase_across_handoffs(b"yield", HANDOFF_TIMEOUT_MS * ticks_per_ms, || {
        probe_yield(pid)
    });

    let spin_iterations = calibrate_spin_iterations(pid, ticks_per_ms);
    run_phase_across_handoffs(b"preemption", HANDOFF_TIMEOUT_MS * ticks_per_ms, || {
        probe_preemption(pid, spin_iterations)
    });

    let linger_end_tsc = start_tsc + LINGER_MS * ticks_per_ms;
    while read_tsc() < linger_end_tsc {
        sys_yield();
    }

    write_bytes(b"regprobe: ok\n");
    sys_exit(0)
}
