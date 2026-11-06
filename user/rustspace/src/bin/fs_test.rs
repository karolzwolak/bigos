#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::arch::global_asm;
use rustspace::{
    CpuInfoFlat, DirEntryFlat, FD_FLAG_READ, FD_FLAG_WRITE, StatFlat, println, sys_close_file,
    sys_create_dir, sys_create_file, sys_delete, sys_exit, sys_get_cpu_info, sys_list_dir,
    sys_open_file, sys_read_file, sys_stat_file, sys_write_file, tsc_read,
};

global_asm!(
    ".section .text.entry",
    ".global _start",
    "_start:",
    "    call main",
    "    ud2",
);

static mut PASS_COUNT: u32 = 0;
static mut FAIL_COUNT: u32 = 0;

macro_rules! expect_true {
    ($label:expr, $cond:expr) => {{
        if $cond {
            unsafe { PASS_COUNT += 1 };
            println!("  PASS  {}", $label);
        } else {
            unsafe { FAIL_COUNT += 1 };
            println!("  FAIL  {}", $label);
        }
    }};
}

macro_rules! expect_eq {
    ($label:expr, $left:expr, $right:expr) => {{
        let l = $left;
        let r = $right;
        if l == r {
            unsafe { PASS_COUNT += 1 };
            println!("  PASS  {}", $label);
        } else {
            unsafe { FAIL_COUNT += 1 };
            println!("  FAIL  {} (got {}, expected {})", $label, l, r);
        }
    }};
}

fn suite_header(name: &str) {
    println!("");
    println!("=== {} ===", name);
}

fn print_summary() {
    let pass = unsafe { PASS_COUNT };
    let fail = unsafe { FAIL_COUNT };
    println!("");
    println!("--- results: {} passed, {} failed ---", pass, fail);
}

// Read all bytes from an open fd into a Vec. Stops at EOF (0 bytes returned).
// All filenames used in this test are valid FAT32 8.3 names:
//   stem <= 8 chars, extension <= 3 chars.
unsafe fn read_all(fd: usize, max_bytes: usize) -> Option<Vec<u8>> {
    let mut result = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let to_read = buf.len().min(max_bytes.saturating_sub(result.len()));
        if to_read == 0 {
            break;
        }
        let n = unsafe { sys_read_file(fd, &mut buf[..to_read]) };
        if n == usize::MAX {
            return None;
        }
        if n == 0 {
            break;
        }
        result.extend_from_slice(&buf[..n]);
    }
    Some(result)
}

unsafe fn suite_stat_and_list() {
    suite_header("stat and list_dir");

    // stat the root
    let mut stat = StatFlat::zeroed();
    let ok = unsafe { sys_stat_file("/", &mut stat) };
    expect_true!("stat('/') succeeds", ok);
    expect_true!("stat('/') is_dir=1", stat.is_dir == 1);

    // list root
    const MAX_ENTRIES: usize = 32;
    let mut entries = [DirEntryFlat::zeroed(); MAX_ENTRIES];
    let count = unsafe { sys_list_dir("/", &mut entries) };
    expect_true!("list_dir('/') succeeds", count != usize::MAX);
    expect_true!("list_dir('/') at least 1 entry", count >= 1);

    if count != usize::MAX {
        println!("  root entries ({}):", count);
        for i in 0..count {
            let e = &entries[i];
            let kind = if e.is_dir == 1 { "DIR " } else { "FILE" };
            println!("    [{}] {} ({} bytes)", kind, e.name_str(), e.size);
        }
    }

    // stat a known-short name — counter.txt exists on ATA disk
    let mut stat2 = StatFlat::zeroed();
    let ok2 = unsafe { sys_stat_file("/counter.txt", &mut stat2) };
    if ok2 {
        expect_true!("stat('/counter.txt') is_dir=0", stat2.is_dir == 0);
    } else {
        expect_true!("stat('/counter.txt') absent: fails cleanly", !ok2);
    }
}

unsafe fn suite_create_write_read_delete() {
    suite_header("create / write / read / delete");

    // 8.3 compliant: stem="rw" (2), ext="txt" (3)
    const PATH: &str = "/rw.txt";
    const WRITE_DATA: &[u8] = b"MofuOS fs test";

    unsafe { sys_delete(PATH) };

    let created = unsafe { sys_create_file(PATH) };
    expect_true!("create_file('/rw.txt')", created);

    let mut stat = StatFlat::zeroed();
    let ok = unsafe { sys_stat_file(PATH, &mut stat) };
    expect_true!("stat after create", ok);
    expect_true!("stat after create: is_dir=0", stat.is_dir == 0);
    expect_eq!("stat after create: size=0", stat.size, 0);

    let fd_w = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    expect_true!("open for write", fd_w != usize::MAX);
    if fd_w == usize::MAX {
        println!("  cannot continue - open failed");
        return;
    }

    let written = unsafe { sys_write_file(fd_w, 0, WRITE_DATA) };
    expect_eq!("write byte count", written, WRITE_DATA.len());
    unsafe { sys_close_file(fd_w) };

    let mut stat2 = StatFlat::zeroed();
    unsafe { sys_stat_file(PATH, &mut stat2) };
    expect_eq!(
        "stat after write: size",
        stat2.size as usize,
        WRITE_DATA.len()
    );

    let fd_r = unsafe { sys_open_file(PATH, FD_FLAG_READ) };
    expect_true!("open for read", fd_r != usize::MAX);
    if fd_r == usize::MAX {
        return;
    }

    let content = unsafe { read_all(fd_r, WRITE_DATA.len() + 64) };
    unsafe { sys_close_file(fd_r) };

    expect_true!("read_all succeeds", content.is_some());
    if let Some(data) = content {
        expect_eq!("read byte count", data.len(), WRITE_DATA.len());
        expect_true!("read content matches", data.as_slice() == WRITE_DATA);
    }

    let deleted = unsafe { sys_delete(PATH) };
    expect_true!("delete file", deleted);

    let mut stat3 = StatFlat::zeroed();
    let still_exists = unsafe { sys_stat_file(PATH, &mut stat3) };
    expect_true!("stat after delete fails", !still_exists);
}

unsafe fn suite_sequential_reads() {
    suite_header("sequential reads");

    // 8.3: stem="seq" (3), ext="txt" (3)
    const PATH: &str = "/seq.txt";
    // exactly 16 bytes, two reads of 8
    const DATA: &[u8] = b"ABCDEFGHIJKLMNOP";

    unsafe { sys_delete(PATH) };
    let created = unsafe { sys_create_file(PATH) };
    if !created {
        println!("  SKIP (create failed)");
        return;
    }

    let fd = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd == usize::MAX {
        println!("  SKIP (open failed)");
        return;
    }
    unsafe { sys_write_file(fd, 0, DATA) };
    unsafe { sys_close_file(fd) };

    let fd_r = unsafe { sys_open_file(PATH, FD_FLAG_READ) };
    expect_true!("open for sequential read", fd_r != usize::MAX);
    if fd_r == usize::MAX {
        return;
    }

    let mut buf_a = [0u8; 8];
    let n1 = unsafe { sys_read_file(fd_r, &mut buf_a) };
    expect_eq!("first read: 8 bytes", n1, 8);
    expect_true!("first read content", &buf_a == b"ABCDEFGH");

    let mut buf_b = [0u8; 8];
    let n2 = unsafe { sys_read_file(fd_r, &mut buf_b) };
    expect_eq!("second read: 8 bytes", n2, 8);
    expect_true!("second read content", &buf_b == b"IJKLMNOP");

    // third read at EOF must return 0, not an error
    let mut buf_c = [0u8; 8];
    let n3 = unsafe { sys_read_file(fd_r, &mut buf_c) };
    expect_true!("third read at EOF returns 0", n3 == 0);

    unsafe { sys_close_file(fd_r) };
    unsafe { sys_delete(PATH) };
}

unsafe fn suite_directories() {
    suite_header("directories");

    // 8.3: stem="testdir" (7), no ext — valid
    const DIR_PATH: &str = "/testdir";
    // 8.3: stem="inside" (6), ext="txt" (3)
    const FILE_IN_DIR: &str = "/testdir/inside.txt";

    unsafe { sys_delete(FILE_IN_DIR) };
    unsafe { sys_delete(DIR_PATH) };

    let dir_created = unsafe { sys_create_dir(DIR_PATH) };
    expect_true!("create_dir('/testdir')", dir_created);

    let mut stat = StatFlat::zeroed();
    let ok = unsafe { sys_stat_file(DIR_PATH, &mut stat) };
    expect_true!("stat('/testdir') succeeds", ok);
    expect_true!("stat('/testdir') is_dir=1", stat.is_dir == 1);

    let file_created = unsafe { sys_create_file(FILE_IN_DIR) };
    expect_true!("create_file inside dir", file_created);

    let fd = unsafe { sys_open_file(FILE_IN_DIR, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd != usize::MAX {
        unsafe { sys_write_file(fd, 0, b"hi subdir") };
        unsafe { sys_close_file(fd) };
    }

    let mut stat2 = StatFlat::zeroed();
    let ok2 = unsafe { sys_stat_file(FILE_IN_DIR, &mut stat2) };
    expect_true!("stat file inside dir", ok2);
    expect_true!("file inside dir is_dir=0", stat2.is_dir == 0);

    const MAX_ENTRIES: usize = 16;
    let mut entries = [DirEntryFlat::zeroed(); MAX_ENTRIES];
    let count = unsafe { sys_list_dir(DIR_PATH, &mut entries) };
    expect_true!("list_dir after adding file", count != usize::MAX);

    // Safe guard against usize::MAX
    if count != usize::MAX {
        let found = (0..count).any(|i| entries[i].name_str() == "INSIDE.TXT");
        expect_true!("inside.txt in dir listing", found);
    }

    unsafe { sys_delete(FILE_IN_DIR) };
    unsafe { sys_delete(DIR_PATH) };
}

unsafe fn suite_error_cases() {
    suite_header("error cases");

    // open non-existent file
    let fd = unsafe { sys_open_file("/nofile.txt", FD_FLAG_READ) };
    expect_true!("open non-existent returns MAX", fd == usize::MAX);

    // stat non-existent path
    let mut stat = StatFlat::zeroed();
    let ok = unsafe { sys_stat_file("/nope.txt", &mut stat) };
    expect_true!("stat non-existent fails", !ok);

    // list a file as directory
    const PATH: &str = "/errtest.txt";
    unsafe { sys_delete(PATH) };
    unsafe { sys_create_file(PATH) };

    let mut entries = [DirEntryFlat::zeroed(); 8];
    let count = unsafe { sys_list_dir(PATH, &mut entries) };
    expect_true!("list_dir on a file fails", count == usize::MAX);

    // close out-of-range fd
    let closed = unsafe { sys_close_file(255) };
    expect_true!("close invalid fd fails", !closed);

    // read from write-only fd
    let fd_w = unsafe { sys_open_file(PATH, FD_FLAG_WRITE) };
    if fd_w != usize::MAX {
        let mut buf = [0u8; 8];
        let n = unsafe { sys_read_file(fd_w, &mut buf) };
        expect_true!("read from write-only fd fails", n == usize::MAX);
        unsafe { sys_close_file(fd_w) };
    }

    // write to read-only fd
    let fd_r = unsafe { sys_open_file(PATH, FD_FLAG_READ) };
    if fd_r != usize::MAX {
        let n = unsafe { sys_write_file(fd_r, 0, b"bad write") };
        expect_true!("write to read-only fd fails", n == usize::MAX);
        unsafe { sys_close_file(fd_r) };
    }

    // invalid 8.3 filename (stem too long: 9 chars)
    let bad = unsafe { sys_create_file("/toolongst.txt") };
    expect_true!("create_file with 9-char stem fails", !bad);

    unsafe { sys_delete(PATH) };
}

unsafe fn suite_overwrite() {
    suite_header("overwrite");

    // 8.3: stem="owtest" (6), ext="txt" (3)
    const PATH: &str = "/owtest.txt";
    const FIRST: &[u8] = b"first write";
    const SECOND: &[u8] = b"second write is longer";

    unsafe { sys_delete(PATH) };
    unsafe { sys_create_file(PATH) };

    let fd = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd == usize::MAX {
        println!("  SKIP (open failed)");
        return;
    }
    unsafe { sys_write_file(fd, 0, FIRST) };
    unsafe { sys_close_file(fd) };

    let fd2 = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd2 == usize::MAX {
        println!("  SKIP (second open failed)");
        return;
    }
    let w = unsafe { sys_write_file(fd2, 0, SECOND) };
    expect_eq!("overwrite byte count", w, SECOND.len());
    unsafe { sys_close_file(fd2) };

    let fd3 = unsafe { sys_open_file(PATH, FD_FLAG_READ) };
    expect_true!("open after overwrite", fd3 != usize::MAX);
    if fd3 == usize::MAX {
        return;
    }
    let content = unsafe { read_all(fd3, SECOND.len() + 64) };
    unsafe { sys_close_file(fd3) };

    expect_true!("read after overwrite succeeds", content.is_some());
    if let Some(data) = content {
        expect_eq!("overwrite: final size", data.len(), SECOND.len());
        expect_true!("overwrite: content matches", data.as_slice() == SECOND);
    }

    unsafe { sys_delete(PATH) };
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    println!("fs_test: starting");

    let mut cpu_info = CpuInfoFlat::zeroed();
    unsafe {
        sys_get_cpu_info(&mut cpu_info);
    }
    let freq_hz = cpu_info.tsc_frequency_hz;
    rustspace::println!("CPU Frequency: {} MHz", freq_hz / 1_000_000);
    let cycles_to_us = |cycles: u64| -> u64 {
        if freq_hz > 0 {
            (cycles as u128 * 1_000_000 / freq_hz as u128) as u64
        } else {
            0
        }
    };

    unsafe {
        let (start_cycle, core) = rustspace::tsc_read();
        rustspace::println!("Core: {}, Cycle: {}", core, start_cycle);

        suite_stat_and_list();
        let (cycle, _) = rustspace::tsc_read();
        let elapsed = cycle - start_cycle;
        rustspace::println!(
            "stat_and_list: {} cycles ({}us)",
            elapsed,
            cycles_to_us(elapsed)
        );

        suite_create_write_read_delete();
        let (cycle, _) = rustspace::tsc_read();
        let elapsed = cycle - start_cycle;
        rustspace::println!(
            "create_write_read_delete: {} cycles ({}us)",
            elapsed,
            cycles_to_us(elapsed)
        );

        suite_sequential_reads();
        let (cycle, _) = rustspace::tsc_read();
        let elapsed = cycle - start_cycle;
        rustspace::println!(
            "sequential_reads: {} cycles ({}us)",
            elapsed,
            cycles_to_us(elapsed)
        );

        suite_directories();
        let (cycle, _) = rustspace::tsc_read();
        let elapsed = cycle - start_cycle;
        rustspace::println!(
            "directories: {} cycles ({}us)",
            elapsed,
            cycles_to_us(elapsed)
        );

        suite_error_cases();
        let (cycle, _) = rustspace::tsc_read();
        let elapsed = cycle - start_cycle;
        rustspace::println!(
            "error_cases: {} cycles ({}us)",
            elapsed,
            cycles_to_us(elapsed)
        );

        suite_overwrite();
        let (cycle, _) = rustspace::tsc_read();
        let elapsed = cycle - start_cycle;
        rustspace::println!(
            "overwrite: {} cycles ({}us)",
            elapsed,
            cycles_to_us(elapsed)
        );

        let total_cycles = cycle - start_cycle;
        rustspace::println!(
            "Total Cycles: {} ({}us)",
            total_cycles,
            cycles_to_us(total_cycles)
        );
    }

    print_summary();
    println!("fs_test: done");

    unsafe { sys_exit(0) }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("fs_test: PANIC");
    unsafe { sys_exit(1) }
}
