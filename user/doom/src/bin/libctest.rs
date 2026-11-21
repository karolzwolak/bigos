#![no_std]
#![no_main]

extern crate alloc;

use core::ffi::{CStr, c_char, c_int};
use core::sync::atomic::Ordering;
use rustspace::job::{JobPoll, ShellJob};
use rustspace::{ArgSlice, Args, program_shared_data, sys_exit, sys_yield};

#[path = "../libc/mod.rs"]
mod libc;

const COUNTER_CHECKS_PASSED: usize = 0;
const COUNTER_CHECKS_FAILED: usize = 1;
const COUNTER_CHECKS_DONE: usize = 2;
const COUNTER_EXIT_CHILD_PID: usize = 3;

const OWN_NAME: &str = "libctest";
const PRINT_MODE: &str = "print";
const EXIT_MODE: &str = "exit";
const PRINT_COMMAND: &str = "libctest print";
const EXIT_COMMAND: &str = "libctest exit";

const LONG_LINE_BYTES: usize = 700;
const CAPTURE_BYTES: usize = 4096;
const POLL_CHUNK_BYTES: usize = 61;
const EXIT_CODE_PANIC: i32 = 255;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys_exit(EXIT_CODE_PANIC)
}

unsafe extern "C" {
    fn libctest_main();
    fn libctest_print();
    fn libctest_exit();
    fn libctest_fill_long_line(buffer: *mut c_char);
}

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {libctest_entry}",
        "ud2",
        libctest_entry = sym libctest_entry,
    )
}

fn check(name: &str, condition: bool) {
    let counters = &program_shared_data().test_counters;
    if condition {
        counters[COUNTER_CHECKS_PASSED].fetch_add(1, Ordering::AcqRel);
        return;
    }

    counters[COUNTER_CHECKS_FAILED].fetch_add(1, Ordering::AcqRel);
    rustspace::println!("{}: FAILED: {}", OWN_NAME, name);
}

// called by the C checks
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libctest_report(name: *const c_char, passed: c_int) {
    // SAFETY: the C side passes string literals
    let name = unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("?");

    check(name, passed != 0);
}

struct Capture {
    bytes: [u8; CAPTURE_BYTES],
    len: usize,
}

impl Capture {
    const fn new() -> Self {
        Self {
            bytes: [0; CAPTURE_BYTES],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Polls like the shell does until the job is gone
fn run_to_completion(job: &mut ShellJob, capture: &mut Capture) {
    let mut chunk = [0u8; POLL_CHUNK_BYTES];

    loop {
        match job.poll(&mut chunk) {
            JobPoll::Output(read_len) => {
                let copy_len = read_len.min(CAPTURE_BYTES - capture.len);
                capture.bytes[capture.len..capture.len + copy_len]
                    .copy_from_slice(&chunk[..copy_len]);
                capture.len += copy_len;
            }
            JobPoll::NothingYet => sys_yield(),
            JobPoll::Finished | JobPoll::NoJob => return,
        }
    }
}

/// Builds what libctest_print writes
fn expected_print_output(out: &mut Capture) {
    let mut line = [0u8; LONG_LINE_BYTES + 1];
    // SAFETY: the buffer holds the line and its NUL
    unsafe { libctest_fill_long_line(line.as_mut_ptr() as *mut c_char) };

    let mut push = |bytes: &[u8]| {
        out.bytes[out.len..out.len + bytes.len()].copy_from_slice(bytes);
        out.len += bytes.len();
    };
    push(&line[..LONG_LINE_BYTES]);
    push(b"|42\nsecond\nend\n!\n");
}

fn check_stdout_through_a_job(job: &mut ShellJob) {
    check("start the print job", job.start(PRINT_COMMAND).is_ok());

    let mut captured = Capture::new();
    run_to_completion(job, &mut captured);

    let mut expected = Capture::new();
    expected_print_output(&mut expected);
    check(
        "printf output longer than the format chunk arrives whole",
        captured.as_bytes() == expected.as_bytes(),
    );
}

fn run_exit_child(job: &mut ShellJob) {
    let started = job.start(EXIT_COMMAND);
    check("start the exit job", started.is_ok());
    if let Ok(pid) = started {
        program_shared_data().test_counters[COUNTER_EXIT_CHILD_PID].store(pid, Ordering::Release);
    }

    run_to_completion(job, &mut Capture::new());
}

extern "C" fn libctest_entry(argc: usize, argv: *const ArgSlice) -> ! {
    // SAFETY: the kernel passes the argc and argv it placed on the stack
    let args = unsafe { Args::from_entry(argc, argv) };

    match args.iter().next() {
        Some(PRINT_MODE) => {
            // SAFETY: plain C function without arguments
            unsafe { libctest_print() };
            sys_exit(0)
        }
        Some(EXIT_MODE) => {
            // SAFETY: plain C function without arguments
            unsafe { libctest_exit() };
            sys_exit(EXIT_CODE_PANIC)
        }
        _ => {}
    }

    // SAFETY: plain C function without arguments
    unsafe { libctest_main() };

    let mut job = ShellJob::Idle;
    check_stdout_through_a_job(&mut job);
    run_exit_child(&mut job);

    program_shared_data().test_counters[COUNTER_CHECKS_DONE].store(1, Ordering::Release);
    sys_exit(0)
}
