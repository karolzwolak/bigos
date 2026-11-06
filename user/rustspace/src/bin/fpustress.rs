#![no_std]
#![no_main]

use core::arch::asm;
use core::arch::x86_64::_rdtsc;
use rustspace::{SYS_YIELD, sys_exit, sys_write};

const XMM_REGISTER_COUNT: usize = 16;
const YIELD_ROUNDS: u64 = 10;
const MXCSR_DEFAULT: u32 = 0x1F80;

const EXIT_CODE_PANIC: i32 = 1;
const EXIT_CODE_DIRTY_INITIAL_XMM: i32 = 2;
const EXIT_CODE_BAD_INITIAL_MXCSR: i32 = 3;
const EXIT_CODE_XMM_LOST_ACROSS_YIELD: i32 = 4;
const EXIT_CODE_GPR_LOST_ACROSS_YIELD: i32 = 5;

const GPR_PROBE_COUNT: usize = 4;
const GPR_PROBE_ROUND_STRIDE: u64 = 0x0101_0101_0101_0101;

#[repr(C, align(16))]
#[derive(Clone, Copy, PartialEq)]
struct XmmBank {
    registers: [u128; XMM_REGISTER_COUNT],
}

macro_rules! store_xmm_bank {
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
        )
    };
}

macro_rules! load_xmm_bank {
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
        )
    };
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { sys_exit(EXIT_CODE_PANIC) }
}

fn write_str(s: &[u8]) {
    unsafe { sys_write(1, s.as_ptr(), s.len()) };
}

#[inline(never)]
unsafe fn yield_with_loaded_xmm(expected_bank: *const XmmBank, actual_bank: *mut XmmBank) {
    unsafe {
        asm!(
            "push rbx",
            "push rbp",
            "push r12",
            "push r13",
            "push r14",
            "push r15",
            "push rdi",
            load_xmm_bank!("rsi"),
            "syscall",
            "pop rdi",
            store_xmm_bank!("rdi"),
            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop rbp",
            "pop rbx",
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

/// Yields with xmm and r12-r15 loaded and relies on the kernel to give them all back
/// Unlike yield_with_loaded_xmm nothing is pushed around the syscall, the registers
/// are operands of the asm so the compiler knows they change
#[inline(never)]
unsafe fn yield_with_loaded_xmm_and_gprs(
    expected_bank: *const XmmBank,
    actual_bank: *mut XmmBank,
    gprs: &mut [u64; GPR_PROBE_COUNT],
) {
    let [mut r12_value, mut r13_value, mut r14_value, mut r15_value] = *gprs;

    unsafe {
        asm!(
            "push rdi",
            load_xmm_bank!("rsi"),
            "syscall",
            "pop rdi",
            store_xmm_bank!("rdi"),
            inout("rax") SYS_YIELD => _,
            inout("rdi") actual_bank => _,
            inout("rsi") expected_bank => _,
            inout("r12") r12_value,
            inout("r13") r13_value,
            inout("r14") r14_value,
            inout("r15") r15_value,
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

    *gprs = [r12_value, r13_value, r14_value, r15_value];
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
    let mut initial_bank = core::mem::MaybeUninit::<XmmBank>::uninit();
    let mut initial_mxcsr: u32 = 0;

    unsafe {
        asm!(
            store_xmm_bank!("rdi"),
            "stmxcsr [rsi]",
            in("rdi") initial_bank.as_mut_ptr(),
            in("rsi") &mut initial_mxcsr as *mut u32,
        );
    }
    let initial_bank = unsafe { initial_bank.assume_init() };

    if initial_bank.registers.iter().any(|&value| value != 0) {
        write_str(b"fpustress: FAIL initial xmm not zero\n");
        unsafe { sys_exit(EXIT_CODE_DIRTY_INITIAL_XMM) }
    }
    if initial_mxcsr != MXCSR_DEFAULT {
        write_str(b"fpustress: FAIL initial mxcsr\n");
        unsafe { sys_exit(EXIT_CODE_BAD_INITIAL_MXCSR) }
    }

    let process_seed = unsafe { _rdtsc() } as u128;

    for round in 0..YIELD_ROUNDS {
        let mut expected_bank = XmmBank {
            registers: [0; XMM_REGISTER_COUNT],
        };
        for (register_index, value) in expected_bank.registers.iter_mut().enumerate() {
            *value = (process_seed << 64) | ((round as u128) << 32) | register_index as u128;
        }

        let mut actual_bank = XmmBank {
            registers: [0; XMM_REGISTER_COUNT],
        };
        unsafe { yield_with_loaded_xmm(&expected_bank, &mut actual_bank) };

        if actual_bank != expected_bank {
            write_str(b"fpustress: FAIL xmm lost across yield\n");
            unsafe { sys_exit(EXIT_CODE_XMM_LOST_ACROSS_YIELD) }
        }
    }

    for round in 0..YIELD_ROUNDS {
        let mut expected_bank = XmmBank {
            registers: [0; XMM_REGISTER_COUNT],
        };
        for (register_index, value) in expected_bank.registers.iter_mut().enumerate() {
            *value = (process_seed << 64) | ((round as u128) << 32) | register_index as u128;
        }

        let mut expected_gprs = [0u64; GPR_PROBE_COUNT];
        for (gpr_index, value) in expected_gprs.iter_mut().enumerate() {
            *value = (process_seed as u64)
                .wrapping_add(GPR_PROBE_ROUND_STRIDE.wrapping_mul(round + gpr_index as u64));
        }

        let mut actual_bank = XmmBank {
            registers: [0; XMM_REGISTER_COUNT],
        };
        let mut actual_gprs = expected_gprs;
        unsafe {
            yield_with_loaded_xmm_and_gprs(&expected_bank, &mut actual_bank, &mut actual_gprs)
        };

        if actual_bank != expected_bank {
            write_str(b"fpustress: FAIL xmm lost across yield without saved gprs\n");
            unsafe { sys_exit(EXIT_CODE_XMM_LOST_ACROSS_YIELD) }
        }
        if actual_gprs != expected_gprs {
            write_str(b"fpustress: FAIL gpr lost across yield\n");
            unsafe { sys_exit(EXIT_CODE_GPR_LOST_ACROSS_YIELD) }
        }
    }

    write_str(b"fpustress: ok\n");
    unsafe { sys_exit(0) }
}
