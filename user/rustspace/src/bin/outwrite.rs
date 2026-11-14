#![no_std]
#![no_main]

use core::sync::atomic::Ordering;
use rustspace::channel::write_stdout;
use rustspace::{ArgSlice, Args, program_shared_data, sys_exit, sys_write, sys_yield};

// Indexes into the shared test counters, test_user_stdout and outread use the same values
const COUNTER_READER_EXITING: usize = 6;
const COUNTER_ORPHAN_WRITES_DONE: usize = 7;

// modes outread starts this program in, the single argument
const MODE_BASIC: &str = "basic";
const MODE_FLOOD: &str = "flood";
const MODE_FLOOD_SYSCALL: &str = "floodsys";
const MODE_ORPHAN: &str = "orphan";

// outread expects exactly this from MODE_BASIC
const BASIC_FIRST_LINE: &str = "outwrite: hello";
const BASIC_LINE_NUMBER: u32 = 1;
const BASIC_STDERR_TEXT: &[u8] = b"outwrite: stderr too\n";

// outread checks every byte of MODE_FLOOD and MODE_FLOOD_SYSCALL against the pattern
const FLOOD_TOTAL_BYTES: usize = 48 * 1024;
const FLOOD_CHUNK_BYTES: usize = 1000;
// prime, so the pattern never lines up with the chunk or the ring size
const FLOOD_PATTERN_PERIOD: usize = 251;

// three times the one page ring, without dropping it would block forever
const ORPHAN_TOTAL_BYTES: usize = 3 * 4096;

const STDOUT_FD: u64 = 1;
const STDERR_FD: u64 = 2;

const EXIT_CODE_PANIC: i32 = 255;
const EXIT_CODE_BAD_ARGS: i32 = 2;
const EXIT_CODE_SHORT_WRITE: i32 = 3;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys_exit(EXIT_CODE_PANIC)
}

// the kernel enters with a 16 byte aligned rsp, the ABI wants it 8 off at function entry
// rdi and rsi still hold argc and argv when outwrite_main is called
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {outwrite_main}",
        "ud2",
        outwrite_main = sym outwrite_main,
    )
}

fn write_all(fd: u64, bytes: &[u8]) {
    let written = unsafe { sys_write(fd, bytes.as_ptr(), bytes.len()) };
    if written != bytes.len() as i64 {
        sys_exit(EXIT_CODE_SHORT_WRITE);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WritePath {
    /// rustspace copies straight into the ring, no syscall
    Direct,
    /// one Write syscall per chunk, the kernel copies into the ring
    Syscall,
}

fn write_pattern(total_bytes: usize, write_path: WritePath) {
    let mut chunk = [0u8; FLOOD_CHUNK_BYTES];
    let mut offset = 0;

    while offset < total_bytes {
        let chunk_len = FLOOD_CHUNK_BYTES.min(total_bytes - offset);
        for (index, byte) in chunk[..chunk_len].iter_mut().enumerate() {
            *byte = ((offset + index) % FLOOD_PATTERN_PERIOD) as u8;
        }
        match write_path {
            WritePath::Direct => write_stdout(&chunk[..chunk_len]),
            WritePath::Syscall => write_all(STDOUT_FD, &chunk[..chunk_len]),
        }

        offset += chunk_len;
    }
}

fn run_orphan() {
    let counters = &program_shared_data().test_counters;
    while counters[COUNTER_READER_EXITING].load(Ordering::Acquire) == 0 {
        sys_yield();
    }

    // the reader is exiting or gone, what does not fit is dropped once it is gone
    write_pattern(ORPHAN_TOTAL_BYTES, WritePath::Direct);
    counters[COUNTER_ORPHAN_WRITES_DONE].store(1, Ordering::Release);
}

extern "C" fn outwrite_main(argc: usize, argv: *const ArgSlice) -> ! {
    let args = unsafe { Args::from_entry(argc, argv) };
    if args.len() != 1 {
        sys_exit(EXIT_CODE_BAD_ARGS);
    }

    match args.get(0) {
        MODE_BASIC => {
            rustspace::println!("{}", BASIC_FIRST_LINE);
            rustspace::println!("outwrite: line {}", BASIC_LINE_NUMBER);
            write_all(STDERR_FD, BASIC_STDERR_TEXT);
        }
        MODE_FLOOD => write_pattern(FLOOD_TOTAL_BYTES, WritePath::Direct),
        MODE_FLOOD_SYSCALL => write_pattern(FLOOD_TOTAL_BYTES, WritePath::Syscall),
        MODE_ORPHAN => run_orphan(),
        _ => sys_exit(EXIT_CODE_BAD_ARGS),
    }

    sys_exit(0)
}
