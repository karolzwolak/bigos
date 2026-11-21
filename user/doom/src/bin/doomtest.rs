#![no_std]
#![no_main]

use core::sync::atomic::Ordering;

use rustspace::job::{JobPoll, ShellJob};
use rustspace::{program_shared_data, sys_exit, sys_yield};

const COUNTER_CHECKS_PASSED: usize = 0;
const COUNTER_CHECKS_FAILED: usize = 1;
const COUNTER_CHECKS_DONE: usize = 2;
const COUNTER_DOOM_PID: usize = 3;

const OWN_NAME: &str = "doomtest";
const DOOM_COMMAND: &str = "doom -timedemo demo1";
const TIMEDEMO_RESULT_PREFIX: &[u8] = b"timed ";
const LINE_BYTES: usize = 256;
const POLL_CHUNK_BYTES: usize = 256;
const NEWLINE: u8 = b'\n';

const EXIT_CODE_PANIC: i32 = 255;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys_exit(EXIT_CODE_PANIC)
}

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {doomtest_main}",
        "ud2",
        doomtest_main = sym doomtest_main,
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

struct LineScanner {
    line: [u8; LINE_BYTES],
    line_len: usize,
    line_count: usize,
    has_timedemo_result: bool,
}

impl LineScanner {
    const fn new() -> Self {
        Self {
            line: [0; LINE_BYTES],
            line_len: 0,
            line_count: 0,
            has_timedemo_result: false,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == NEWLINE {
                self.finish_line();
                continue;
            }

            if self.line_len < LINE_BYTES {
                self.line[self.line_len] = byte;
                self.line_len += 1;
            }
        }
    }

    fn finish_line(&mut self) {
        self.has_timedemo_result |= self.line[..self.line_len].starts_with(TIMEDEMO_RESULT_PREFIX);
        self.line_len = 0;
        self.line_count += 1;
    }
}

fn run_to_completion(job: &mut ShellJob, scanner: &mut LineScanner) {
    let mut chunk = [0u8; POLL_CHUNK_BYTES];

    loop {
        match job.poll(&mut chunk) {
            JobPoll::Output(read_len) => scanner.feed(&chunk[..read_len]),
            JobPoll::NothingYet => sys_yield(),
            JobPoll::Finished | JobPoll::NoJob => return,
        }
    }
}

extern "C" fn doomtest_main() -> ! {
    let mut job = ShellJob::Idle;

    let started = job.start(DOOM_COMMAND);
    check("start doom", started.is_ok());
    if let Ok(doom_pid) = started {
        program_shared_data().test_counters[COUNTER_DOOM_PID].store(doom_pid, Ordering::Release);

        let mut scanner = LineScanner::new();
        run_to_completion(&mut job, &mut scanner);

        check("the job is idle once doom is gone", !job.is_running());
        check("doom printed its startup lines", scanner.line_count > 1);
        check(
            "doom printed the timedemo result",
            scanner.has_timedemo_result,
        );
    }

    program_shared_data().test_counters[COUNTER_CHECKS_DONE].store(1, Ordering::Release);
    sys_exit(0)
}
