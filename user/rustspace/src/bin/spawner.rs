#![no_std]
#![no_main]

use core::sync::atomic::Ordering;
use rustspace::{
    FD_FLAG_READ, MAX_ARGC, MAX_ARGV_BYTES, MAX_PROCESS_NAME_LEN, ProcessCreateFlags,
    ProcessCreateInfo, SYS_CREATE_PROCESS, SyscallError, program_shared_data, sys_create_process,
    sys_exit, sys_get_pid, sys_open_file, sys_yield, syscall1,
};

// Indexes into the shared test counters, test_user_spawn, spawnee and treeleaf use the same values
// fs_test uses 0..=3 and fsworker 4..=6, which never run in the same test kernel
const COUNTER_DETACHED_PID: usize = 7;
const COUNTER_RUNNING: usize = 8;
const COUNTER_CHECKS_PASSED: usize = 9;
const COUNTER_CHECKS_FAILED: usize = 10;
const COUNTER_CHILD_PID: usize = 11;
const COUNTER_RELEASE: usize = 13;
const COUNTER_CHECKS_DONE: usize = 14;

const EXIT_CODE_PANIC: i32 = 255;
const EXIT_CODE_CHECKS_FAILED: i32 = 1;

const SPAWNEE_PATH: &str = "/BIN/spawnee";
const SPAWNEE_NAME: &str = "kid";
const DETACHED_PATH: &str = "/BIN/treeleaf";
const MISSING_PATH: &str = "/BIN/missing";
const TEXT_FILE_PATH: &str = "/HELLO.TXT";
const DIRECTORY_PATH: &str = "/BIN";
const INHERITED_FILE_PATH: &str = "/HELLO.TXT";

// spawnee expects exactly these, followed by the inherited fd number
const FIXED_ARGS: &[u8] = b"alpha\0b c\0\0";
const FIXED_ARG_COUNT: u32 = 3;
const MAX_FD_DIGITS: usize = 10;
const DECIMAL_BASE: usize = 10;

const CHILD_PRIORITY: u8 = 5;
const INVALID_PRIORITY: u8 = 100;
const UNKNOWN_FLAG_BIT: u32 = 1 << 31;
const KERNEL_SPACE_POINTER: u64 = 0xFFFF_8000_0000_1000;
// above the kernel MAX_PATH_LEN of 256
const OVERLONG_PATH_BYTES: usize = 300;
const OVERLONG_NAME: &str = "a_process_name_longer_than_32_bytes";
const NON_UTF8_ARGS: &[u8] = b"\xff\0";
const UNTERMINATED_ARGS: &[u8] = b"alpha\0b c";
const UNTERMINATED_ARG_COUNT: u32 = 2;

static_assertions::const_assert!(OVERLONG_NAME.len() > MAX_PROCESS_NAME_LEN);

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
        "call {spawner_main}",
        "ud2",
        spawner_main = sym spawner_main,
    )
}

fn check(name: &str, condition: bool) {
    let counters = &program_shared_data().test_counters;
    if condition {
        counters[COUNTER_CHECKS_PASSED].fetch_add(1, Ordering::AcqRel);
        return;
    }

    counters[COUNTER_CHECKS_FAILED].fetch_add(1, Ordering::AcqRel);
    rustspace::println!("spawner: FAILED: {}", name);
}

fn expect_error(name: &str, info: &ProcessCreateInfo, expected: SyscallError) {
    check(name, sys_create_process(info) == Err(expected));
}

/// Appends value in decimal and a NUL, returns the new length
fn push_decimal_arg(buffer: &mut [u8], mut length: usize, mut value: usize) -> usize {
    let mut digits = [0u8; MAX_FD_DIGITS];
    let mut digit_count = 0;

    loop {
        digits[digit_count] = b'0' + (value % DECIMAL_BASE) as u8;
        digit_count += 1;
        value /= DECIMAL_BASE;
        if value == 0 {
            break;
        }
    }

    for index in (0..digit_count).rev() {
        buffer[length] = digits[index];
        length += 1;
    }
    buffer[length] = 0;

    length + 1
}

fn run_spawn_checks() {
    let counters = &program_shared_data().test_counters;

    let fd = sys_open_file(INHERITED_FILE_PATH, FD_FLAG_READ);
    check("open the file the child inherits", fd.is_ok());
    let fd = fd.unwrap_or(0);

    let mut args = [0u8; FIXED_ARGS.len() + MAX_FD_DIGITS + 1];
    args[..FIXED_ARGS.len()].copy_from_slice(FIXED_ARGS);
    let args_len = push_decimal_arg(&mut args, FIXED_ARGS.len(), fd);

    // the invalid priority proves INHERIT_PRIORITY ignores the field
    let child_info = ProcessCreateInfo::new(SPAWNEE_PATH, INVALID_PRIORITY)
        .with_name(SPAWNEE_NAME)
        .with_args(&args[..args_len], FIXED_ARG_COUNT + 1)
        .with_flags(ProcessCreateFlags::INHERIT_FDS | ProcessCreateFlags::INHERIT_PRIORITY);
    let child_result = sys_create_process(&child_info);
    check(
        "spawn a named child with args, fds and priority",
        matches!(child_result, Ok(pid) if pid as usize != sys_get_pid()),
    );
    if let Ok(pid) = child_result {
        counters[COUNTER_CHILD_PID].store(pid, Ordering::Release);
    }

    let detached_info = ProcessCreateInfo::new(DETACHED_PATH, CHILD_PRIORITY)
        .with_flags(ProcessCreateFlags::DETACHED);
    let detached_result = sys_create_process(&detached_info);
    check("spawn a detached child", detached_result.is_ok());
    if let Ok(pid) = detached_result {
        counters[COUNTER_DETACHED_PID].store(pid, Ordering::Release);
    }
}

fn run_path_error_checks() {
    expect_error(
        "spawn of a missing file",
        &ProcessCreateInfo::new(MISSING_PATH, CHILD_PRIORITY),
        SyscallError::NOT_FOUND,
    );
    expect_error(
        "spawn of a text file",
        &ProcessCreateInfo::new(TEXT_FILE_PATH, CHILD_PRIORITY),
        SyscallError::INVALID_EXECUTABLE,
    );
    expect_error(
        "spawn of a directory",
        &ProcessCreateInfo::new(DIRECTORY_PATH, CHILD_PRIORITY),
        SyscallError::IS_A_DIRECTORY,
    );

    let overlong_path = [b'a'; OVERLONG_PATH_BYTES];
    let overlong_path = core::str::from_utf8(&overlong_path).unwrap_or("");
    expect_error(
        "spawn with an overlong path",
        &ProcessCreateInfo::new(overlong_path, CHILD_PRIORITY),
        SyscallError::INVALID_PATH,
    );
}

fn run_info_error_checks() {
    let valid = ProcessCreateInfo::new(SPAWNEE_PATH, CHILD_PRIORITY);

    expect_error(
        "spawn with an invalid priority",
        &ProcessCreateInfo::new(SPAWNEE_PATH, INVALID_PRIORITY),
        SyscallError::INVALID_ARGUMENT,
    );

    let mut unknown_flag = valid;
    unknown_flag.flags = UNKNOWN_FLAG_BIT;
    expect_error(
        "spawn with an unknown flag",
        &unknown_flag,
        SyscallError::INVALID_ARGUMENT,
    );

    let mut reserved_set = valid;
    reserved_set.reserved[0] = 1;
    expect_error(
        "spawn with reserved bytes set",
        &reserved_set,
        SyscallError::INVALID_ARGUMENT,
    );

    expect_error(
        "spawn with a name over the limit",
        &valid.with_name(OVERLONG_NAME),
        SyscallError::INVALID_ARGUMENT,
    );

    let info_ret = unsafe { syscall1(SYS_CREATE_PROCESS, KERNEL_SPACE_POINTER) };
    check(
        "spawn with the info in kernel space",
        info_ret == SyscallError::INVALID_PTR.0.wrapping_neg(),
    );

    let mut kernel_path = valid;
    kernel_path.path_ptr = KERNEL_SPACE_POINTER;
    expect_error(
        "spawn with a kernel space path pointer",
        &kernel_path,
        SyscallError::INVALID_PTR,
    );

    let mut kernel_args = valid.with_args(FIXED_ARGS, FIXED_ARG_COUNT);
    kernel_args.argv_ptr = KERNEL_SPACE_POINTER;
    expect_error(
        "spawn with a kernel space argv pointer",
        &kernel_args,
        SyscallError::INVALID_PTR,
    );
}

fn run_args_error_checks() {
    let valid = ProcessCreateInfo::new(SPAWNEE_PATH, CHILD_PRIORITY);

    expect_error(
        "spawn with argc not matching the strings",
        &valid.with_args(FIXED_ARGS, FIXED_ARG_COUNT + 1),
        SyscallError::INVALID_ARGUMENT,
    );
    expect_error(
        "spawn with args not ending in NUL",
        &valid.with_args(UNTERMINATED_ARGS, UNTERMINATED_ARG_COUNT),
        SyscallError::INVALID_ARGUMENT,
    );
    expect_error(
        "spawn with a non UTF-8 arg",
        &valid.with_args(NON_UTF8_ARGS, 1),
        SyscallError::INVALID_ARGUMENT,
    );

    let oversized_args = [0u8; MAX_ARGV_BYTES + 1];
    expect_error(
        "spawn with args over the size limit",
        &valid.with_args(&oversized_args, oversized_args.len() as u32),
        SyscallError::INVALID_ARGUMENT,
    );

    let too_many_args = [0u8; MAX_ARGC + 1];
    expect_error(
        "spawn with more args than the limit",
        &valid.with_args(&too_many_args, too_many_args.len() as u32),
        SyscallError::INVALID_ARGUMENT,
    );
}

extern "C" fn spawner_main() -> ! {
    let counters = &program_shared_data().test_counters;
    counters[COUNTER_RUNNING].store(1, Ordering::Release);

    run_spawn_checks();
    run_path_error_checks();
    run_info_error_checks();
    run_args_error_checks();

    counters[COUNTER_CHECKS_DONE].store(1, Ordering::Release);

    // stays alive, with the inherited file open, so the kernel test sees it as the parent
    while counters[COUNTER_RELEASE].load(Ordering::Acquire) == 0 {
        sys_yield();
    }

    if counters[COUNTER_CHECKS_FAILED].load(Ordering::Acquire) != 0 {
        sys_exit(EXIT_CODE_CHECKS_FAILED);
    }
    sys_exit(0)
}
