#![no_std]
#![no_main]

use core::sync::atomic::Ordering;
use rustspace::{
    ArgSlice, Args, program_shared_data, sys_exit, sys_get_pid, sys_read_file_at, sys_yield,
};

// Indexes into the shared test counters, test_user_spawn and spawner use the same values
// fs_test uses 0..=3 and fsworker 4..=6, which never run in the same test kernel
const COUNTER_SPAWNEE_PID: usize = 12;
const COUNTER_RELEASE: usize = 13;
const COUNTER_SPAWNEE_ARGS: usize = 15;

// values of COUNTER_SPAWNEE_ARGS
const ARGS_OK: u32 = 1;
const ARGS_WRONG: u32 = 2;

// spawner passes these, then the number of the fd it opened on /HELLO.TXT
const EXPECTED_FIXED_ARGS: [&str; 3] = ["alpha", "b c", ""];
const EXPECTED_ARG_COUNT: usize = EXPECTED_FIXED_ARGS.len() + 1;
const FD_ARG_INDEX: usize = EXPECTED_FIXED_ARGS.len();
const INHERITED_FILE_CONTENT: &[u8] = b"hello";
const DECIMAL_BASE: usize = 10;

const EXIT_CODE_PANIC: i32 = 255;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys_exit(EXIT_CODE_PANIC)
}

// the kernel enters with a 16 byte aligned rsp, the ABI wants it 8 off at function entry
// rdi and rsi still hold argc and argv when spawnee_main is called
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {spawnee_main}",
        "ud2",
        spawnee_main = sym spawnee_main,
    )
}

fn parse_decimal(text: &str) -> Option<usize> {
    if text.is_empty() {
        return None;
    }

    let mut value = 0usize;
    for digit in text.bytes() {
        if !digit.is_ascii_digit() {
            return None;
        }
        value = value * DECIMAL_BASE + (digit - b'0') as usize;
    }

    Some(value)
}

fn args_are_expected(args: &Args) -> bool {
    if args.len() != EXPECTED_ARG_COUNT {
        rustspace::println!("spawnee: got {} args", args.len());
        return false;
    }
    if !EXPECTED_FIXED_ARGS
        .iter()
        .enumerate()
        .all(|(index, expected)| args.get(index) == *expected)
    {
        return false;
    }

    // the fd was opened by the parent, the child only has it through INHERIT_FDS
    let Some(fd) = parse_decimal(args.get(FD_ARG_INDEX)) else {
        return false;
    };
    let mut content = [0u8; INHERITED_FILE_CONTENT.len()];
    matches!(sys_read_file_at(fd, 0, &mut content), Ok(read) if read == content.len())
        && content == INHERITED_FILE_CONTENT
}

/// Checks its args and the inherited fd, reports its pid, then yields until released
extern "C" fn spawnee_main(argc: usize, argv: *const ArgSlice) -> ! {
    let args = unsafe { Args::from_entry(argc, argv) };
    let counters = &program_shared_data().test_counters;

    let args_state = if args_are_expected(&args) {
        ARGS_OK
    } else {
        ARGS_WRONG
    };
    counters[COUNTER_SPAWNEE_ARGS].store(args_state, Ordering::Release);
    counters[COUNTER_SPAWNEE_PID].store(sys_get_pid() as u32, Ordering::Release);

    while counters[COUNTER_RELEASE].load(Ordering::Acquire) == 0 {
        sys_yield();
    }

    sys_exit(0)
}
