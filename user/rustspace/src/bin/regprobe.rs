#![no_std]
#![no_main]

use core::arch::asm;
use core::mem::offset_of;
use rustspace::{SYS_ECHO, SYS_YIELD, sys_exit, sys_write};

const PROBE_ROUNDS: u32 = 16;
const HEX_DIGIT_COUNT: usize = 16;
const BITS_PER_HEX_DIGIT: usize = 4;
const DECIMAL_BASE: u8 = 10;

const EXIT_CODE_PANIC: i32 = 255;
const EXIT_CODE_LOST_REGISTER_ON_SYSRET: i32 = 100;
const EXIT_CODE_LOST_REGISTER_ON_YIELD: i32 = 101;
const EXIT_CODE_YIELD_RETURNED_NONZERO: i32 = 102;

/// Registers a syscall must preserve, rcx and r11 are clobbered by the instruction itself
#[repr(C)]
#[derive(Clone, Copy)]
struct RegisterSet {
    rax: u64,
    rbx: u64,
    rbp: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    r8: u64,
    r9: u64,
    r10: u64,
}

const REGISTER_COUNT: usize = core::mem::size_of::<RegisterSet>() / core::mem::size_of::<u64>();

const REGISTER_NAMES: [&[u8]; REGISTER_COUNT] = [
    b"rax", b"rbx", b"rbp", b"r12", b"r13", b"r14", b"r15", b"rdi", b"rsi", b"rdx", b"r8", b"r9",
    b"r10",
];

// the value of rax is the syscall result, so it carries no sentinel
const SENTINELS: RegisterSet = RegisterSet {
    rax: 0,
    rbx: 0x1111_1111_1111_1111,
    rbp: 0x2222_2222_2222_2222,
    r12: 0x3333_3333_3333_3333,
    r13: 0x4444_4444_4444_4444,
    r14: 0x5555_5555_5555_5555,
    r15: 0x6666_6666_6666_6666,
    rdi: 0x7777_7777_7777_7777,
    rsi: 0x8888_8888_8888_8888,
    rdx: 0x9999_9999_9999_9999,
    r8: 0xAAAA_AAAA_AAAA_AAAA,
    r9: 0xBBBB_BBBB_BBBB_BBBB,
    r10: 0xCCCC_CCCC_CCCC_CCCC,
};

static mut SYSCALL_NUMBER: u64 = 0;
static mut OBSERVED: RegisterSet = SENTINELS;

impl RegisterSet {
    fn as_array(&self) -> [u64; REGISTER_COUNT] {
        [
            self.rax, self.rbx, self.rbp, self.r12, self.r13, self.r14, self.r15, self.rdi,
            self.rsi, self.rdx, self.r8, self.r9, self.r10,
        ]
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { sys_exit(EXIT_CODE_PANIC) }
}

/// Loads a sentinel into every preserved register, issues the syscall and records
/// what the registers hold afterwards. rbx and rbp cannot be asm operands, so they
/// are saved by hand
#[inline(never)]
unsafe fn probe(syscall_number: u64) {
    unsafe {
        SYSCALL_NUMBER = syscall_number;

        asm!(
            "push rbx",
            "push rbp",
            "push r12",
            "push r13",
            "push r14",
            "push r15",

            "mov rbx, {rbx_value}",
            "mov rbp, {rbp_value}",
            "mov r12, {r12_value}",
            "mov r13, {r13_value}",
            "mov r14, {r14_value}",
            "mov r15, {r15_value}",
            "mov rdi, {rdi_value}",
            "mov rsi, {rsi_value}",
            "mov rdx, {rdx_value}",
            "mov r8, {r8_value}",
            "mov r9, {r9_value}",
            "mov r10, {r10_value}",
            "mov rax, [rip + {number}]",
            "syscall",

            "mov [rip + {observed} + {rax_offset}], rax",
            "mov [rip + {observed} + {rbx_offset}], rbx",
            "mov [rip + {observed} + {rbp_offset}], rbp",
            "mov [rip + {observed} + {r12_offset}], r12",
            "mov [rip + {observed} + {r13_offset}], r13",
            "mov [rip + {observed} + {r14_offset}], r14",
            "mov [rip + {observed} + {r15_offset}], r15",
            "mov [rip + {observed} + {rdi_offset}], rdi",
            "mov [rip + {observed} + {rsi_offset}], rsi",
            "mov [rip + {observed} + {rdx_offset}], rdx",
            "mov [rip + {observed} + {r8_offset}], r8",
            "mov [rip + {observed} + {r9_offset}], r9",
            "mov [rip + {observed} + {r10_offset}], r10",

            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop rbp",
            "pop rbx",

            number = sym SYSCALL_NUMBER,
            observed = sym OBSERVED,
            rbx_value = const SENTINELS.rbx,
            rbp_value = const SENTINELS.rbp,
            r12_value = const SENTINELS.r12,
            r13_value = const SENTINELS.r13,
            r14_value = const SENTINELS.r14,
            r15_value = const SENTINELS.r15,
            rdi_value = const SENTINELS.rdi,
            rsi_value = const SENTINELS.rsi,
            rdx_value = const SENTINELS.rdx,
            r8_value = const SENTINELS.r8,
            r9_value = const SENTINELS.r9,
            r10_value = const SENTINELS.r10,
            rax_offset = const offset_of!(RegisterSet, rax),
            rbx_offset = const offset_of!(RegisterSet, rbx),
            rbp_offset = const offset_of!(RegisterSet, rbp),
            r12_offset = const offset_of!(RegisterSet, r12),
            r13_offset = const offset_of!(RegisterSet, r13),
            r14_offset = const offset_of!(RegisterSet, r14),
            r15_offset = const offset_of!(RegisterSet, r15),
            rdi_offset = const offset_of!(RegisterSet, rdi),
            rsi_offset = const offset_of!(RegisterSet, rsi),
            rdx_offset = const offset_of!(RegisterSet, rdx),
            r8_offset = const offset_of!(RegisterSet, r8),
            r9_offset = const offset_of!(RegisterSet, r9),
            r10_offset = const offset_of!(RegisterSet, r10),
            out("rax") _,
            out("rcx") _,
            out("rdx") _,
            out("rsi") _,
            out("rdi") _,
            out("r8") _,
            out("r9") _,
            out("r10") _,
            out("r11") _,
        );
    }
}

fn write_bytes(bytes: &[u8]) {
    unsafe { sys_write(1, bytes.as_ptr(), bytes.len()) };
}

fn write_hex(value: u64) {
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

    write_bytes(&digits);
}

/// Returns how many preserved registers changed, rax is judged by the caller
fn count_lost_registers(label: &[u8]) -> u32 {
    let observed = unsafe { core::ptr::addr_of!(OBSERVED).read() }.as_array();
    let expected = SENTINELS.as_array();
    let mut lost_count = 0;

    for register_index in 1..REGISTER_COUNT {
        if observed[register_index] == expected[register_index] {
            continue;
        }

        lost_count += 1;
        write_bytes(b"regprobe: ");
        write_bytes(label);
        write_bytes(b" lost ");
        write_bytes(REGISTER_NAMES[register_index]);
        write_bytes(b"=");
        write_hex(observed[register_index]);
        write_bytes(b"\n");
    }

    lost_count
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    for _ in 0..PROBE_ROUNDS {
        unsafe { probe(SYS_ECHO) };
        if count_lost_registers(b"sysret") != 0 {
            unsafe { sys_exit(EXIT_CODE_LOST_REGISTER_ON_SYSRET) }
        }

        unsafe { probe(SYS_YIELD) };
        if count_lost_registers(b"yield") != 0 {
            unsafe { sys_exit(EXIT_CODE_LOST_REGISTER_ON_YIELD) }
        }

        let yield_result = unsafe { core::ptr::addr_of!(OBSERVED).read() }.rax;
        if yield_result != 0 {
            write_bytes(b"regprobe: yield returned ");
            write_hex(yield_result);
            write_bytes(b"\n");
            unsafe { sys_exit(EXIT_CODE_YIELD_RETURNED_NONZERO) }
        }
    }

    write_bytes(b"regprobe: ok\n");
    unsafe { sys_exit(0) }
}
