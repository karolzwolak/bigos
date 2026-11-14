#![no_std]
#![no_main]

use core::sync::atomic::Ordering;
use rustspace::job::{JobPoll, ShellJob};
use rustspace::{BIGPROC_PID, SyscallError, program_shared_data, sys_exit, sys_get_pid, sys_yield};

// Indexes into the shared test counters, test_user_run uses the same values
const COUNTER_CHECKS_PASSED: usize = 0;
const COUNTER_CHECKS_FAILED: usize = 1;
const COUNTER_CHECKS_DONE: usize = 2;
const COUNTER_LSPROC_PID: usize = 3;

const OWN_NAME: &str = "runjob";
const LSPROC_NAME: &str = "lsproc";
const BIGPROC_NAME: &str = "bigproc";
const LSPROC_HEADER: &str = "PID NAME";

const CAPTURE_BYTES: usize = 4096;
const POLL_CHUNK_BYTES: usize = 61;

// what outwrite prints in its basic mode, stdout and stderr share the channel
const EXPECTED_BASIC_OUTPUT: &[u8] = b"outwrite: hello\noutwrite: line 1\noutwrite: stderr too\n";

const EXIT_CODE_PANIC: i32 = 255;

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
        "call {runjob_main}",
        "ud2",
        runjob_main = sym runjob_main,
    )
}

fn check(name: &str, condition: bool) {
    let counters = &program_shared_data().test_counters;
    if condition {
        counters[COUNTER_CHECKS_PASSED].fetch_add(1, Ordering::AcqRel);
        return;
    }

    counters[COUNTER_CHECKS_FAILED].fetch_add(1, Ordering::AcqRel);
    rustspace::println!("runjob: FAILED: {}", name);
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

    fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).unwrap_or("")
    }
}

/// Polls like the shell does until the job is gone, small reads so the ring end is crossed
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

/// True when some `PID NAME` line of the listing names this process
fn lists_process(listing: &str, pid: usize, name: &str) -> bool {
    listing.lines().skip(1).any(|line| {
        let mut columns = line.split_ascii_whitespace();
        columns
            .next()
            .and_then(|column| column.parse::<usize>().ok())
            == Some(pid)
            && columns.next() == Some(name)
    })
}

fn every_row_is_pid_and_name(listing: &str) -> bool {
    listing.lines().skip(1).all(|line| {
        let mut columns = line.split_ascii_whitespace();
        columns
            .next()
            .is_some_and(|column| column.parse::<usize>().is_ok())
            && columns.next().is_some()
            && columns.next().is_none()
    })
}

fn check_lsproc(job: &mut ShellJob, command_line: &str) {
    let started = job.start(command_line);
    check("start lsproc", started.is_ok());
    let Ok(lsproc_pid) = started else {
        return;
    };
    check("the job runs", job.is_running());
    program_shared_data().test_counters[COUNTER_LSPROC_PID].store(lsproc_pid, Ordering::Release);

    let mut capture = Capture::new();
    run_to_completion(job, &mut capture);
    check("the job is idle once lsproc is gone", !job.is_running());
    check(
        "an idle job polls as NoJob",
        job.poll(&mut [0u8; POLL_CHUNK_BYTES]) == JobPoll::NoJob,
    );

    let listing = capture.as_str();
    check(
        "lsproc prints its header first",
        listing.lines().next().map(str::trim) == Some(LSPROC_HEADER),
    );
    check(
        "every lsproc row is a pid and a name",
        every_row_is_pid_and_name(listing),
    );
    check(
        "lsproc lists runjob",
        lists_process(listing, sys_get_pid(), OWN_NAME),
    );
    check(
        "lsproc lists itself",
        lists_process(listing, lsproc_pid as usize, LSPROC_NAME),
    );
    check(
        "lsproc lists bigproc",
        lists_process(listing, BIGPROC_PID, BIGPROC_NAME),
    );
}

fn check_arguments_reach_the_program(job: &mut ShellJob) {
    check(
        "start outwrite with an argument",
        job.start("outwrite basic").is_ok(),
    );

    let mut capture = Capture::new();
    run_to_completion(job, &mut capture);
    check(
        "outwrite output arrives exactly",
        capture.as_bytes() == EXPECTED_BASIC_OUTPUT,
    );
}

fn check_bad_command_lines(job: &mut ShellJob) {
    check(
        "an unknown program is not found",
        job.start("nosuchpr") == Err(SyscallError::NOT_FOUND),
    );
    check("a failed start leaves the job idle", !job.is_running());
    check(
        "an empty command line is rejected",
        job.start("  ") == Err(SyscallError::INVALID_ARGUMENT),
    );
}

extern "C" fn runjob_main() -> ! {
    let mut job = ShellJob::Idle;

    check_lsproc(&mut job, LSPROC_NAME);
    check_lsproc(&mut job, "/BIN/lsproc");
    check_arguments_reach_the_program(&mut job);
    check_bad_command_lines(&mut job);

    program_shared_data().test_counters[COUNTER_CHECKS_DONE].store(1, Ordering::Release);
    sys_exit(0)
}
