#![no_std]
#![no_main]

use core::fmt::Write;
use core::sync::atomic::Ordering;
use rustspace::{
    DirEntryFlat, FD_FLAG_READ, FD_FLAG_WRITE, File, StatFlat, program_shared_data, sys_create_dir,
    sys_create_file, sys_delete, sys_exit, sys_get_pid, sys_list_dir, sys_stat_file, sys_yield,
};

// Indexes into the shared test counters, the kernel tests use the same values
const COUNTER_RUNNING: usize = 0;
const COUNTER_CHECKS_PASSED: usize = 1;
const COUNTER_CHECKS_FAILED: usize = 2;
const COUNTER_WORKER_MODE: usize = 4;
const COUNTER_WORKERS_DONE: usize = 5;
const COUNTER_HELD_FDS: usize = 6;

const MODE_WORK: u32 = 0;
const MODE_HOLD: u32 = 1;

const EXIT_CODE_PANIC: i32 = 255;
const EXIT_CODE_CHECKS_FAILED: i32 = 1;

const PATTERN_PERIOD: usize = 251;
const ROUNDS: usize = 20;
const FILES_PER_ROUND: usize = 3;
const FILE_SIZES_BYTES: [usize; FILES_PER_ROUND] = [300, 2000, 20_000];
const LIST_BUFFER_ENTRIES: usize = 8;
const PATH_BUFFER_BYTES: usize = 32;

const HOLD_PATH: &str = "/HOLD.BIN";
const HOLD_FD_COUNT: u32 = 4;
const HOLD_WRITE_BYTES: usize = 4096;

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
        "call {worker_main}",
        "ud2",
        worker_main = sym worker_main,
    )
}

struct PathBuffer {
    bytes: [u8; PATH_BUFFER_BYTES],
    len: usize,
}

impl PathBuffer {
    fn new() -> Self {
        Self {
            bytes: [0; PATH_BUFFER_BYTES],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl Write for PathBuffer {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let end = self.len + text.len();
        if end > PATH_BUFFER_BYTES {
            return Err(core::fmt::Error);
        }

        self.bytes[self.len..end].copy_from_slice(text.as_bytes());
        self.len = end;

        Ok(())
    }
}

struct Checker {
    failed_locally: bool,
}

impl Checker {
    fn check(&mut self, name: &str, condition: bool) {
        let counters = &program_shared_data().test_counters;
        if condition {
            counters[COUNTER_CHECKS_PASSED].fetch_add(1, Ordering::AcqRel);
            return;
        }

        self.failed_locally = true;
        counters[COUNTER_CHECKS_FAILED].fetch_add(1, Ordering::AcqRel);
        rustspace::println!("fsworker: FAILED: {}", name);
    }
}

fn pattern_byte(index: usize, seed: usize) -> u8 {
    ((index + seed) % PATTERN_PERIOD) as u8
}

fn fill_pattern(buffer: &mut [u8], seed: usize) {
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = pattern_byte(index, seed);
    }
}

fn matches_pattern(buffer: &[u8], seed: usize) -> bool {
    buffer
        .iter()
        .enumerate()
        .all(|(index, byte)| *byte == pattern_byte(index, seed))
}

fn run_round(checker: &mut Checker, pid: usize, round: usize) {
    let mut directory = PathBuffer::new();
    let _ = write!(directory, "/W{}", pid);

    checker.check(
        "create worker directory",
        sys_create_dir(directory.as_str()).is_ok(),
    );

    let mut scratch = [0u8; 20_000];
    let mut read_back = [0u8; 20_000];

    for (file_index, size_bytes) in FILE_SIZES_BYTES.iter().enumerate() {
        let mut path = PathBuffer::new();
        let _ = write!(path, "/W{}/F{}", pid, file_index);
        let seed = pid + round + file_index;

        checker.check("create worker file", sys_create_file(path.as_str()).is_ok());
        let Ok(file) = File::open(path.as_str(), FD_FLAG_READ | FD_FLAG_WRITE) else {
            checker.check("open worker file", false);
            continue;
        };

        fill_pattern(&mut scratch[..*size_bytes], seed);
        checker.check(
            "write worker file",
            file.write_at(0, &scratch[..*size_bytes]) == Ok(*size_bytes),
        );
        let read = file.read_at(0, &mut read_back[..*size_bytes]);
        checker.check("read worker file length", read == Ok(*size_bytes));
        checker.check(
            "read worker file content",
            matches_pattern(&read_back[..*size_bytes], seed),
        );
    }

    let mut entries = [DirEntryFlat::zeroed(); LIST_BUFFER_ENTRIES];
    let count = sys_list_dir(directory.as_str(), 0, &mut entries);
    checker.check("list worker directory", count == Ok(FILES_PER_ROUND));

    let mut stat = StatFlat::zeroed();
    let mut path = PathBuffer::new();
    let _ = write!(path, "/W{}/F{}", pid, FILES_PER_ROUND - 1);
    checker.check(
        "stat worker file",
        sys_stat_file(path.as_str(), &mut stat).is_ok(),
    );
    checker.check(
        "stat worker file size",
        stat.size == FILE_SIZES_BYTES[FILES_PER_ROUND - 1] as u64,
    );

    for file_index in 0..FILES_PER_ROUND {
        let mut path = PathBuffer::new();
        let _ = write!(path, "/W{}/F{}", pid, file_index);
        checker.check("delete worker file", sys_delete(path.as_str()).is_ok());
    }
    checker.check(
        "delete worker directory",
        sys_delete(directory.as_str()).is_ok(),
    );
}

fn work() -> ! {
    let pid = sys_get_pid();
    let mut checker = Checker {
        failed_locally: false,
    };

    for round in 0..ROUNDS {
        run_round(&mut checker, pid, round);
    }

    program_shared_data().test_counters[COUNTER_WORKERS_DONE].fetch_add(1, Ordering::AcqRel);

    if checker.failed_locally {
        sys_exit(EXIT_CODE_CHECKS_FAILED)
    }

    sys_exit(0)
}

/// Opens fds, then rewrites a file until the kernel test kills the process
fn hold() -> ! {
    let counters = &program_shared_data().test_counters;
    let mut buffer = [0u8; HOLD_WRITE_BYTES];
    fill_pattern(&mut buffer, 0);

    if sys_create_file(HOLD_PATH).is_err() {
        sys_exit(EXIT_CODE_CHECKS_FAILED)
    }

    let mut held_files = [const { None }; HOLD_FD_COUNT as usize];
    for slot in held_files.iter_mut() {
        match File::open(HOLD_PATH, FD_FLAG_READ | FD_FLAG_WRITE) {
            Ok(file) => *slot = Some(file),
            Err(_) => sys_exit(EXIT_CODE_CHECKS_FAILED),
        }
    }

    counters[COUNTER_HELD_FDS].store(HOLD_FD_COUNT, Ordering::Release);

    let Some(file) = held_files[0].as_ref() else {
        sys_exit(EXIT_CODE_CHECKS_FAILED)
    };
    loop {
        if file.write_at(0, &buffer) != Ok(HOLD_WRITE_BYTES) {
            sys_exit(EXIT_CODE_CHECKS_FAILED)
        }
        sys_yield();
    }
}

extern "C" fn worker_main() -> ! {
    let counters = &program_shared_data().test_counters;
    let mode = counters[COUNTER_WORKER_MODE].load(Ordering::Acquire);
    counters[COUNTER_RUNNING].fetch_add(1, Ordering::AcqRel);

    match mode {
        MODE_HOLD => hold(),
        MODE_WORK => work(),
        _ => sys_exit(EXIT_CODE_CHECKS_FAILED),
    }
}
