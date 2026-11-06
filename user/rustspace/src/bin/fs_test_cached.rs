#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::arch::global_asm;
use rustspace::{
    ALLOCATOR, CacheImportance, CacheStatsFlat, CpuInfoFlat, DirEntryFlat, FD_FLAG_READ,
    FD_FLAG_WRITE, StatFlat, println, sys_allocate, sys_close_file, sys_create_dir,
    sys_create_file, sys_delete, sys_evict_directory, sys_exit, sys_flush_file_cache,
    sys_get_cache_stats, sys_get_cpu_info, sys_list_dir, sys_open_file, sys_pin_file,
    sys_read_file, sys_reserve_cache, sys_stat_file, sys_unpin_file, sys_write_file, tsc_read,
};

global_asm!(
    ".section .text.entry",
    ".global _start",
    "_start:",
    "    call main",
    "    ud2",
);

/// this is optional cause writes take a lot of time
/// to have this make any sense the total file size of the pressure test files must exceed max file cache size
#[cfg(feature = "big_files")]
const BIG_FILES_FILE_SIZE: usize = 6 * 1024 * 1024; // 8 MB
#[cfg(feature = "big_files")]
const BIG_FILES_CHUNK_SIZE: usize = 1 * 1024 * 1024; // 1 MB
#[cfg(not(feature = "big_files"))]
const BIG_FILES_FILE_SIZE: usize = 12 * 1024; // 12 KB
#[cfg(not(feature = "big_files"))]
const BIG_FILES_CHUNK_SIZE: usize = 4096;
const BIG_FILES_FILE_COUNT: usize = 3;

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

// Helper: read all bytes from an open fd into a Vec
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

// Helper: print cache statistics
unsafe fn print_cache_stats(label: &str) {
    let mut stats = CacheStatsFlat::zeroed();
    let ok = unsafe { sys_get_cache_stats(&mut stats) };
    if ok {
        println!(
            "  [cache] {}: {} files ({} dirty), {} KB / {} KB",
            label,
            stats.total_files,
            stats.dirty_files,
            stats.total_bytes / 1024,
            stats.max_bytes / 1024,
        );
    } else {
        println!("  [cache] {}: stats unavailable", label);
    }
}

// Helper: measure cycles for a closure
unsafe fn measure_cycles<F: FnOnce()>(label: &str, f: F) -> u64 {
    let (start, _) = unsafe { tsc_read() };
    f();
    let (end, _) = unsafe { tsc_read() };
    let elapsed = end - start;
    println!("  [perf] {}: {} cycles", label, elapsed);
    elapsed
}

// ===== Suite 1: Basic Cache Operations =====

unsafe fn suite_cache_basics() {
    suite_header("cache basics");

    // Verify cache is working by checking stats
    unsafe { print_cache_stats("initial") };

    // Reserve cache space for test directory with high importance
    let ok = unsafe { sys_reserve_cache("/cactest", CacheImportance::VeryHigh as u8) };
    expect_true!("reserve_cache('/cactest')", ok == 0);

    const DIR_PATH: &str = "/cactest";
    const FILE_PATH: &str = "/cactest/pinned.txt";
    const FILE_DATA: &[u8] = b"This file should stay in cache";

    // Clean up from previous runs
    unsafe { sys_delete(FILE_PATH) };
    unsafe { sys_delete(DIR_PATH) };
    unsafe { sys_create_dir(DIR_PATH) };
    unsafe { sys_create_file(FILE_PATH) };

    // Write test data
    let fd = unsafe { sys_open_file(FILE_PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd != usize::MAX {
        unsafe { sys_write_file(fd, 0, FILE_DATA) };
        unsafe { sys_close_file(fd) };
    }

    // Pin the file
    let ok = unsafe { sys_pin_file(FILE_PATH) };
    expect_true!("pin_file succeeds", ok == 0);

    unsafe { print_cache_stats("after pin") };

    // First read: should load into cache
    let (start_cold, _) = unsafe { tsc_read() };
    let fd1 = unsafe { sys_open_file(FILE_PATH, FD_FLAG_READ) };
    if fd1 != usize::MAX {
        let _data = unsafe { read_all(fd1, FILE_DATA.len()) };
        unsafe { sys_close_file(fd1) };
    }
    let (end_cold, _) = unsafe { tsc_read() };
    let cold_cycles = end_cold - start_cold;
    println!("  [perf] first read (cold cache): {} cycles", cold_cycles);

    // Second read: should hit cache, much faster
    let (start_hot, _) = unsafe { tsc_read() };
    let fd2 = unsafe { sys_open_file(FILE_PATH, FD_FLAG_READ) };
    if fd2 != usize::MAX {
        let _data = unsafe { read_all(fd2, FILE_DATA.len()) };
        unsafe { sys_close_file(fd2) };
    }
    let (end_hot, _) = unsafe { tsc_read() };
    let hot_cycles = end_hot - start_hot;
    println!("  [perf] second read (warm cache): {} cycles", hot_cycles);

    // Cache should speed up reads significantly
    if cold_cycles > 0 {
        let speedup = cold_cycles as f64 / hot_cycles.max(1) as f64;
        println!("  [perf] cache speedup: {:.1}x", speedup);
        expect_true!("cache provides speedup", speedup > 1.0);
    }

    // Unpin the file
    let ok = unsafe { sys_unpin_file(FILE_PATH) };
    expect_true!("unpin_file succeeds", ok == 0);

    unsafe { print_cache_stats("after unpin") };

    // Clean up
    unsafe { sys_delete(FILE_PATH) };
    unsafe { sys_delete(DIR_PATH) };
}

// ===== Suite 2: Repeated Reads (Cache Hit Rate) =====

unsafe fn suite_repeated_reads() {
    suite_header("repeated reads");

    const PATH: &str = "/reptest.txt";
    const DATA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ012345";

    unsafe { sys_delete(PATH) };
    unsafe { sys_create_file(PATH) };

    let fd = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd == usize::MAX {
        println!("  SKIP (create failed)");
        return;
    }
    unsafe { sys_write_file(fd, 0, DATA) };
    unsafe { sys_close_file(fd) };

    // Reserve cache for this file
    unsafe { sys_reserve_cache("/", CacheImportance::Normal as u8) };

    // Read the file multiple times and measure
    const ITERATIONS: usize = 10;
    let mut total_cycles = 0u64;
    let mut first_cycles = 0u64;

    for i in 0..ITERATIONS {
        let (start, _) = unsafe { tsc_read() };
        let fd_r = unsafe { sys_open_file(PATH, FD_FLAG_READ) };
        if fd_r == usize::MAX {
            println!("  SKIP (open failed at iteration {})", i);
            break;
        }
        let content = unsafe { read_all(fd_r, DATA.len()) };
        unsafe { sys_close_file(fd_r) };
        let (end, _) = unsafe { tsc_read() };
        let elapsed = end - start;
        total_cycles += elapsed;

        if i == 0 {
            first_cycles = elapsed;
            println!("  [perf] iteration 0 (cold): {} cycles", elapsed);
        }

        // Verify content
        if let Some(data) = &content {
            if data.as_slice() != DATA {
                println!("  FAIL iteration {}: data mismatch", i);
                return;
            }
        }
    }

    let avg_hot = if ITERATIONS > 1 {
        (total_cycles - first_cycles) / (ITERATIONS as u64 - 1)
    } else {
        0
    };

    println!("  [perf] average hot read: {} cycles", avg_hot);
    println!(
        "  [perf] first read overhead: {} cycles",
        first_cycles - avg_hot
    );

    expect_true!("repeated reads consistent", total_cycles > 0);

    unsafe { print_cache_stats("after repeated reads") };
    unsafe { sys_delete(PATH) };
}

// ===== Suite 3: Directory Eviction =====

unsafe fn suite_directory_eviction() {
    suite_header("directory eviction");

    const DIR: &str = "/evitest";
    const FILES: &[&str] = &["/evitest/a.txt", "/evitest/b.txt", "/evitest/c.txt"];
    const DATA: &[u8] = b"eviction test data file";

    // Setup
    unsafe { sys_delete(FILES[2]) };
    unsafe { sys_delete(FILES[1]) };
    unsafe { sys_delete(FILES[0]) };
    unsafe { sys_delete(DIR) };
    unsafe { sys_create_dir(DIR) };

    // Create and write files
    for path in FILES {
        unsafe { sys_create_file(path) };
        let fd = unsafe { sys_open_file(path, FD_FLAG_READ | FD_FLAG_WRITE) };
        if fd != usize::MAX {
            unsafe { sys_write_file(fd, 0, DATA) };
            unsafe { sys_close_file(fd) };
        }
    }

    // Read all files into cache
    for path in FILES {
        let fd = unsafe { sys_open_file(path, FD_FLAG_READ) };
        if fd != usize::MAX {
            let _ = unsafe { read_all(fd, DATA.len()) };
            unsafe { sys_close_file(fd) };
        }
    }

    unsafe { print_cache_stats("before eviction") };

    // Evict entire directory
    let freed = unsafe { sys_evict_directory(DIR) };
    expect_true!("evict_directory returns bytes freed", freed > 0);
    println!("  evicted {} bytes from cache", freed);

    unsafe { print_cache_stats("after eviction") };

    // Verify files still readable (from disk)
    for path in FILES {
        let fd = unsafe { sys_open_file(path, FD_FLAG_READ) };
        expect_true!("file still readable after eviction", fd != usize::MAX);
        if fd != usize::MAX {
            let content = unsafe { read_all(fd, DATA.len()) };
            if let Some(data) = content {
                expect_true!("content intact", data.as_slice() == DATA);
            }
            unsafe { sys_close_file(fd) };
        }
    }

    // Re-read should re-cache
    let (start_recache, _) = unsafe { tsc_read() };
    for path in FILES {
        let fd = unsafe { sys_open_file(path, FD_FLAG_READ) };
        if fd != usize::MAX {
            let _ = unsafe { read_all(fd, DATA.len()) };
            unsafe { sys_close_file(fd) };
        }
    }
    let (end_recache, _) = unsafe { tsc_read() };
    println!(
        "  [perf] re-cache after eviction: {} cycles",
        end_recache - start_recache
    );

    // Cleanup
    for path in FILES {
        unsafe { sys_delete(path) };
    }
    unsafe { sys_delete(DIR) };
}

// ===== Suite 5: Standard Tests (from original) =====

unsafe fn suite_stat_and_list() {
    suite_header("stat and list_dir");

    let mut stat = StatFlat::zeroed();
    let ok = unsafe { sys_stat_file("/", &mut stat) };
    expect_true!("stat('/') succeeds", ok);
    expect_true!("stat('/') is_dir=1", stat.is_dir == 1);

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

    // First read (cold cache)
    let (cold_start, _) = unsafe { tsc_read() };
    let fd_r = unsafe { sys_open_file(PATH, FD_FLAG_READ) };
    expect_true!("open for read", fd_r != usize::MAX);
    if fd_r == usize::MAX {
        return;
    }
    let content = unsafe { read_all(fd_r, WRITE_DATA.len() + 64) };
    unsafe { sys_close_file(fd_r) };
    let (cold_end, _) = unsafe { tsc_read() };

    expect_true!("read_all succeeds", content.is_some());
    if let Some(data) = content {
        expect_eq!("read byte count", data.len(), WRITE_DATA.len());
        expect_true!("read content matches", data.as_slice() == WRITE_DATA);
    }

    // Second read (should be cached)
    let (warm_start, _) = unsafe { tsc_read() };
    let fd_r2 = unsafe { sys_open_file(PATH, FD_FLAG_READ) };
    if fd_r2 != usize::MAX {
        let _ = unsafe { read_all(fd_r2, WRITE_DATA.len() + 64) };
        unsafe { sys_close_file(fd_r2) };
    }
    let (warm_end, _) = unsafe { tsc_read() };

    println!("  [perf] cold read: {} cycles", cold_end - cold_start);
    println!("  [perf] warm read: {} cycles", warm_end - warm_start);

    let deleted = unsafe { sys_delete(PATH) };
    expect_true!("delete file", deleted);

    let mut stat3 = StatFlat::zeroed();
    let still_exists = unsafe { sys_stat_file(PATH, &mut stat3) };
    expect_true!("stat after delete fails", !still_exists);
}

unsafe fn suite_sequential_reads() {
    suite_header("sequential reads");

    const PATH: &str = "/seq.txt";
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

    let mut buf_c = [0u8; 8];
    let n3 = unsafe { sys_read_file(fd_r, &mut buf_c) };
    expect_true!("third read at EOF returns 0", n3 == 0);

    unsafe { sys_close_file(fd_r) };
    unsafe { sys_delete(PATH) };
}

unsafe fn suite_directories() {
    suite_header("directories");

    const DIR_PATH: &str = "/testdir";
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

    if count != usize::MAX {
        let found = (0..count).any(|i| entries[i].name_str() == "INSIDE.TXT");
        expect_true!("inside.txt in dir listing", found);
    }

    unsafe { sys_delete(FILE_IN_DIR) };
    unsafe { sys_delete(DIR_PATH) };
}

unsafe fn suite_error_cases() {
    suite_header("error cases");

    let fd = unsafe { sys_open_file("/nofile.txt", FD_FLAG_READ) };
    expect_true!("open non-existent returns MAX", fd == usize::MAX);

    let mut stat = StatFlat::zeroed();
    let ok = unsafe { sys_stat_file("/nope.txt", &mut stat) };
    expect_true!("stat non-existent fails", !ok);

    const PATH: &str = "/errtest.txt";
    unsafe { sys_delete(PATH) };
    unsafe { sys_create_file(PATH) };

    let mut entries = [DirEntryFlat::zeroed(); 8];
    let count = unsafe { sys_list_dir(PATH, &mut entries) };
    expect_true!("list_dir on a file fails", count == usize::MAX);

    let closed = unsafe { sys_close_file(255) };
    expect_true!("close invalid fd fails", !closed);

    let fd_w = unsafe { sys_open_file(PATH, FD_FLAG_WRITE) };
    if fd_w != usize::MAX {
        let mut buf = [0u8; 8];
        let n = unsafe { sys_read_file(fd_w, &mut buf) };
        expect_true!("read from write-only fd fails", n == usize::MAX);
        unsafe { sys_close_file(fd_w) };
    }

    let fd_r = unsafe { sys_open_file(PATH, FD_FLAG_READ) };
    if fd_r != usize::MAX {
        let n = unsafe { sys_write_file(fd_r, 0, b"bad write") };
        expect_true!("write to read-only fd fails", n == usize::MAX);
        unsafe { sys_close_file(fd_r) };
    }

    let bad = unsafe { sys_create_file("/toolongst.txt") };
    expect_true!("create_file with 9-char stem fails", !bad);

    unsafe { sys_delete(PATH) };
}

unsafe fn suite_overwrite() {
    suite_header("overwrite");

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

unsafe fn suite_write_throughput() {
    suite_header("write throughput: cached vs uncached");

    const PATH: &str = "/wrthrpt.txt";
    const CHUNK: &[u8] = b"0123456789ABCDEF";
    const CHUNK_SIZE: usize = CHUNK.len();
    const ITERATIONS: usize = 64;

    unsafe { sys_delete(PATH) };
    unsafe { sys_create_file(PATH) };

    let fd = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd == usize::MAX {
        println!("  SKIP (open failed)");
        return;
    }

    let (start_cached, _) = unsafe { tsc_read() };
    let mut offset = 0usize;
    for _ in 0..ITERATIONS {
        unsafe { sys_write_file(fd, offset, CHUNK) };
        offset += CHUNK_SIZE;
    }
    let (end_cached, _) = unsafe { tsc_read() };
    let cached_cycles = end_cached - start_cached;
    let cached_per_write = cached_cycles / ITERATIONS as u64;

    println!(
        "  [perf] {} cached writes: {} cycles total, {} cycles/write",
        ITERATIONS, cached_cycles, cached_per_write
    );

    unsafe { print_cache_stats("after cached writes") };

    let (start_flush, _) = unsafe { tsc_read() };
    let flushed = unsafe { sys_flush_file_cache() };
    let (end_flush, _) = unsafe { tsc_read() };
    let flush_cycles = end_flush - start_flush;

    expect_true!("flush succeeds", flushed);
    println!(
        "  [perf] flush {} bytes to disk: {} cycles",
        CHUNK.len() * ITERATIONS,
        flush_cycles
    );

    unsafe { print_cache_stats("after flush") };

    unsafe { sys_close_file(fd) };
    unsafe { sys_delete(PATH) };
}

unsafe fn suite_sequential_write_batching() {
    suite_header("sequential write batching");

    const PATH_BATCH: &str = "/wbatch.txt";
    const PATH_EAGER: &str = "/weager.txt";
    const CHUNK: &[u8] = b"DATADATADATADATA";
    const CHUNK_SIZE: usize = CHUNK.len();
    const ITERATIONS: usize = 32;

    // write all, flush once
    unsafe { sys_delete(PATH_BATCH) };
    unsafe { sys_create_file(PATH_BATCH) };

    let fd_batch = unsafe { sys_open_file(PATH_BATCH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd_batch == usize::MAX {
        println!("  SKIP (batch open failed)");
        return;
    }

    let (start_batch, _) = unsafe { tsc_read() };
    let mut offset = 0usize;
    for _ in 0..ITERATIONS {
        unsafe { sys_write_file(fd_batch, offset, CHUNK) };
        offset += CHUNK_SIZE;
    }
    unsafe { sys_flush_file_cache() };
    let (end_batch, _) = unsafe { tsc_read() };
    let batch_cycles = end_batch - start_batch;

    unsafe { sys_close_file(fd_batch) };

    // open, write, flush, close per iteration
    unsafe { sys_delete(PATH_EAGER) };
    unsafe { sys_create_file(PATH_EAGER) };

    let (start_eager, _) = unsafe { tsc_read() };
    let mut offset = 0usize;
    for _ in 0..ITERATIONS {
        let fd = unsafe { sys_open_file(PATH_EAGER, FD_FLAG_READ | FD_FLAG_WRITE) };
        if fd == usize::MAX {
            break;
        }
        unsafe { sys_write_file(fd, offset, CHUNK) };
        offset += CHUNK_SIZE;
        unsafe { sys_flush_file_cache() };
        unsafe { sys_close_file(fd) };
    }
    let (end_eager, _) = unsafe { tsc_read() };
    let eager_cycles = end_eager - start_eager;

    println!(
        "  [perf] batched ({} writes + 1 flush): {} cycles, {} cycles/write",
        ITERATIONS,
        batch_cycles,
        batch_cycles / ITERATIONS as u64
    );
    println!(
        "  [perf] eager ({} write+flush pairs): {} cycles, {} cycles/write",
        ITERATIONS,
        eager_cycles,
        eager_cycles / ITERATIONS as u64
    );

    if eager_cycles > 0 {
        let pct = batch_cycles * 100 / eager_cycles;
        println!("  [perf] batched is {}% of eager cost", pct);
        expect_true!("batched cheaper than eager", batch_cycles < eager_cycles);
    }

    // Verify both files have correct size
    let expected_size = (CHUNK.len() * ITERATIONS) as u64;
    let mut stat = StatFlat::zeroed();
    unsafe { sys_stat_file(PATH_BATCH, &mut stat) };
    expect_eq!("batch file size", stat.size, expected_size);

    let mut stat2 = StatFlat::zeroed();
    unsafe { sys_stat_file(PATH_EAGER, &mut stat2) };
    expect_eq!("eager file size", stat2.size, expected_size);

    unsafe { sys_delete(PATH_BATCH) };
    unsafe { sys_delete(PATH_EAGER) };
}

unsafe fn suite_dirty_state() {
    suite_header("dirty state tracking");

    const PATH: &str = "/dirty.txt";

    unsafe { sys_delete(PATH) };
    unsafe { sys_create_file(PATH) };

    // Check initial dirty count
    let mut stats = CacheStatsFlat::zeroed();
    unsafe { sys_get_cache_stats(&mut stats) };
    let dirty_before = stats.dirty_files;
    println!("  dirty files before write: {}", dirty_before);

    // Open and write, should create a dirty cache entry
    let fd = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd == usize::MAX {
        println!("  SKIP (open failed)");
        return;
    }
    unsafe { sys_write_file(fd, 0, b"dirty content") };

    let mut stats2 = CacheStatsFlat::zeroed();
    unsafe { sys_get_cache_stats(&mut stats2) };
    println!("  dirty files after write: {}", stats2.dirty_files);
    expect_true!(
        "dirty count increased after write",
        stats2.dirty_files > dirty_before
    );

    // Flush, dirty count should drop
    let flushed = unsafe { sys_flush_file_cache() };
    expect_true!("flush succeeds", flushed);

    let mut stats3 = CacheStatsFlat::zeroed();
    unsafe { sys_get_cache_stats(&mut stats3) };
    println!("  dirty files after flush: {}", stats3.dirty_files);
    expect_true!(
        "dirty count cleared after flush",
        stats3.dirty_files < stats2.dirty_files
    );

    // Verify content persisted
    unsafe { sys_close_file(fd) };
    let fd2 = unsafe { sys_open_file(PATH, FD_FLAG_READ) };
    if fd2 != usize::MAX {
        let mut buf = [0u8; 16];
        let n = unsafe { sys_read_file(fd2, &mut buf) };
        expect_eq!("read back: correct length", n, b"dirty content".len());
        expect_true!("read back: content matches", &buf[..n] == b"dirty content");
        unsafe { sys_close_file(fd2) };
    }

    unsafe { sys_delete(PATH) };
}

// Write to a file that is at the arena tail, then extend it.
// Should extend in-place without compaction.
unsafe fn suite_arena_grow_at_tail() {
    suite_header("arena grow: in-place tail extension");

    const PATH: &str = "/growtl.txt";
    const INITIAL: &[u8] = b"AAAABBBBCCCCDDDD";
    const EXTENSION: &[u8] = b"EEEEFFFF";
    const EXPECTED: &[u8] = b"AAAABBBBCCCCDDDDEEEEFFFF";

    unsafe { sys_delete(PATH) };
    unsafe { sys_create_file(PATH) };

    let fd = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd == usize::MAX {
        println!("  SKIP (open failed)");
        return;
    }

    let w = unsafe { sys_write_file(fd, 0, INITIAL) };
    expect_eq!("initial write", w, INITIAL.len());

    // Write extension starting at INITIAL.len() — grows the file
    unsafe { sys_close_file(fd) };
    let fd2 = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd2 == usize::MAX {
        println!("  SKIP (reopen failed)");
        return;
    }
    // Advance to the end
    let mut dummy = [0u8; 16];
    let _ = unsafe { sys_read_file(fd2, &mut dummy) };
    // Now write at offset == INITIAL.len()
    let w2 = unsafe { sys_write_file(fd2, INITIAL.len() as usize, EXTENSION) };
    expect_eq!("extension write", w2, EXTENSION.len());
    unsafe { sys_close_file(fd2) };

    unsafe { sys_flush_file_cache() };

    // Verify full content
    let fd3 = unsafe { sys_open_file(PATH, FD_FLAG_READ) };
    expect_true!("open for verify", fd3 != usize::MAX);
    if fd3 != usize::MAX {
        let content = unsafe { read_all(fd3, EXPECTED.len() + 16) };
        unsafe { sys_close_file(fd3) };
        expect_true!("read succeeds", content.is_some());
        if let Some(data) = content {
            expect_eq!("grown file size", data.len(), EXPECTED.len());
            expect_true!("grown file content", data.as_slice() == EXPECTED);
        }
    }

    unsafe { sys_delete(PATH) };
}

// Put file A in cache, then file B, so file A is NOT at the tail.
// Then grow file A, should move it to the tail without compaction.
unsafe fn suite_arena_grow_in_middle() {
    suite_header("arena grow: greedy move from middle");

    const PATH_A: &str = "/gmida.txt";
    const PATH_B: &str = "/gmidb.txt";
    const DATA_A: &[u8] = b"FILEATEXT";
    const EXTRA_A: &[u8] = b"EXTENDED";
    const DATA_B: &[u8] = b"FILEBTEXTHERE";

    unsafe { sys_delete(PATH_A) };
    unsafe { sys_delete(PATH_B) };
    unsafe { sys_create_file(PATH_A) };
    unsafe { sys_create_file(PATH_B) };

    // Write A then B to get A before B in the arena
    let fd_a = unsafe { sys_open_file(PATH_A, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd_a != usize::MAX {
        unsafe { sys_write_file(fd_a, 0, DATA_A) };
        unsafe { sys_close_file(fd_a) };
    }
    let fd_b = unsafe { sys_open_file(PATH_B, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd_b != usize::MAX {
        unsafe { sys_write_file(fd_b, 0, DATA_B) };
        unsafe { sys_close_file(fd_b) };
    }

    unsafe { print_cache_stats("after loading A and B") };

    // Now grow A while B is at the tail
    let fd_a2 = unsafe { sys_open_file(PATH_A, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd_a2 == usize::MAX {
        println!("  SKIP (reopen A failed)");
        unsafe { sys_delete(PATH_A) };
        unsafe { sys_delete(PATH_B) };
        return;
    }
    // Seek past existing content
    let mut skip = [0u8; 9];
    let _ = unsafe { sys_read_file(fd_a2, &mut skip) };
    let w = unsafe { sys_write_file(fd_a2, 9, EXTRA_A) };
    expect_eq!("growing A while B is in middle", w, EXTRA_A.len());
    unsafe { sys_close_file(fd_a2) };

    unsafe { sys_flush_file_cache() };

    // Verify A has correct full content
    let fd_ar = unsafe { sys_open_file(PATH_A, FD_FLAG_READ) };
    expect_true!("open A for verify", fd_ar != usize::MAX);
    if fd_ar != usize::MAX {
        let content = unsafe { read_all(fd_ar, DATA_A.len() + EXTRA_A.len() + 8) };
        unsafe { sys_close_file(fd_ar) };
        if let Some(data) = content {
            expect_eq!("A grown size", data.len(), DATA_A.len() + EXTRA_A.len());
            expect_true!("A prefix intact", &data[..DATA_A.len()] == DATA_A);
            expect_true!("A extension correct", &data[DATA_A.len()..] == EXTRA_A);
        }
    }

    // Verify B is still intact
    let fd_br = unsafe { sys_open_file(PATH_B, FD_FLAG_READ) };
    expect_true!("open B for verify", fd_br != usize::MAX);
    if fd_br != usize::MAX {
        let content = unsafe { read_all(fd_br, DATA_B.len() + 8) };
        unsafe { sys_close_file(fd_br) };
        if let Some(data) = content {
            expect_eq!("B size unchanged", data.len(), DATA_B.len());
            expect_true!("B content intact", data.as_slice() == DATA_B);
        }
    }

    unsafe { sys_delete(PATH_A) };
    unsafe { sys_delete(PATH_B) };
}

// Write 32 bytes, then write 8 bytes at offset 20 — bytes [0..20) must survive.
unsafe fn suite_partial_write_preserves_prefix() {
    suite_header("partial write preserves prefix");

    const PATH: &str = "/partial.txt";

    unsafe { sys_delete(PATH) };
    unsafe { sys_create_file(PATH) };

    let fd = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd == usize::MAX {
        println!("  SKIP (open failed)");
        return;
    }

    // Write 32 bytes of known pattern
    let initial: [u8; 32] = {
        let mut a = [0u8; 32];
        for i in 0..32 {
            a[i] = b'A' + i as u8;
        }
        a
    };
    let w = unsafe { sys_write_file(fd, 0, &initial) };
    expect_eq!("initial 32-byte write", w, 32);
    unsafe { sys_close_file(fd) };

    let fd2 = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd2 == usize::MAX {
        println!("  SKIP (reopen failed)");
        return;
    }
    // Seek to byte 28
    let mut skip = [0u8; 28];
    let _ = unsafe { sys_read_file(fd2, &mut skip) };
    // Write 8 bytes starting at offset 28 -> new file size = 36
    let patch: [u8; 8] = *b"XXXXXXXX";
    let w2 = unsafe { sys_write_file(fd2, 28, &patch) };
    expect_eq!("grow write at offset 28", w2, 8);
    unsafe { sys_close_file(fd2) };

    unsafe { sys_flush_file_cache() };

    // Read back and check prefix [0..28) is unchanged
    let fd3 = unsafe { sys_open_file(PATH, FD_FLAG_READ) };
    expect_true!("open for verify", fd3 != usize::MAX);
    if fd3 != usize::MAX {
        let content = unsafe { read_all(fd3, 64) };
        unsafe { sys_close_file(fd3) };
        if let Some(data) = content {
            expect_eq!("file size after grow", data.len(), 36);
            expect_true!("prefix [0..28) intact", &data[..28] == &initial[..28]);
            expect_true!("patch [28..36] correct", &data[28..36] == &patch);
        }
    }

    unsafe { sys_delete(PATH) };
}

// The arena is 16 MB. Create 3 files of 6 MB each (18 MB total).
// Writing them sequentially fills and overflows the arena, forcing
// eviction of earlier file data. Then verify all three read back
// correctly from disk, and dirty data flushed cleanly.
#[cfg(feature = "test_big_files")]
unsafe fn suite_eviction_under_pressure() {
    suite_header("eviction under pressure");

    // 6 MB each, 3 files = 18 MB > 16 MB arena
    const FILE_SIZE: usize = BIG_FILES_FILE_SIZE;
    const CHUNK_SIZE: usize = BIG_FILES_CHUNK_SIZE;
    const CHUNKS_PER_FILE: usize = FILE_SIZE / CHUNK_SIZE;

    const PATH_A: &str = "/evpa.txt";
    const PATH_B: &str = "/evpb.txt";
    const PATH_C: &str = "/evpc.txt";
    const PATHS: [&str; BIG_FILES_FILE_COUNT] = [PATH_A, PATH_B, PATH_C];
    const FILL: [u8; BIG_FILES_FILE_COUNT] = [0xAA, 0xBB, 0xCC];

    for path in &PATHS {
        unsafe { sys_delete(path) };
    }

    // Create and write all three files sequentially.
    // By the time file C is written, the cache will have evicted parts of A.
    let mut chunk_buf = alloc::vec![0u8; CHUNK_SIZE];
    for (i, path) in PATHS.iter().enumerate() {
        let created = unsafe { sys_create_file(path) };
        if !created {
            println!("  SKIP (create {} failed)", path);
            return;
        }
        let fd = unsafe { sys_open_file(path, FD_FLAG_READ | FD_FLAG_WRITE) };
        if fd == usize::MAX {
            println!("  SKIP (open {} failed)", path);
            return;
        }
        chunk_buf.fill(FILL[i]);
        let (start, _) = unsafe { tsc_read() };
        let mut offset = 0usize;
        for _ in 0..CHUNKS_PER_FILE {
            unsafe { sys_write_file(fd, offset, &chunk_buf) };
            offset += CHUNK_SIZE;
        }
        let (end, _) = unsafe { tsc_read() };
        println!(
            "  wrote {} bytes to {} in {} cycles",
            FILE_SIZE,
            path,
            end - start
        );
        unsafe { sys_close_file(fd) };
    }

    unsafe { print_cache_stats("after 3x6MB writes (arena overflowed, evictions happened)") };

    // Flush all dirty data, at this point some entries may have been evicted+flushed already by
    // evict_one, but some may still be dirty
    let ok = unsafe { sys_flush_file_cache() };
    expect_true!("flush 18 MB succeeds", ok);

    unsafe { print_cache_stats("after flush") };

    // Read back all three files chunk-by-chunk and verify byte values.
    // A was the first in, so it was most likely evicted and will reload from disk.
    let mut read_buf = alloc::vec![0u8; FILE_SIZE];
    for (i, path) in PATHS.iter().enumerate() {
        let (start, _) = unsafe { tsc_read() };
        let fd = unsafe { sys_open_file(path, FD_FLAG_READ) };
        expect_true!("open for verify", fd != usize::MAX);
        if fd == usize::MAX {
            continue;
        }

        let mut total_read = 0usize;
        let mut content_ok = true;
        //chunk_buf.clear();
        read_buf.clear();
        read_buf.set_len(FILE_SIZE);

        let mut file_stat = StatFlat::zeroed();
        unsafe { sys_stat_file(path, &mut file_stat) };
        let file_size_stat = file_stat.size as usize;
        println!("  file size in stat: {}, {} bytes", path, file_size_stat);

        let n = unsafe { sys_read_file(fd, &mut read_buf) };
        println!("  read {} bytes from {}", n, path);
        if n == usize::MAX {
            content_ok = false;
        }
        if read_buf[..n].iter().any(|&b| b != FILL[i]) {
            content_ok = false;
            println!(
                "  ERROR: file {} content mismatch at offset {}",
                path, total_read
            );
        }
        total_read += n;
        unsafe { sys_close_file(fd) };
        let (end, _) = unsafe { tsc_read() };

        expect_true!("file content intact after eviction", content_ok);
        expect_eq!("file size correct", total_read, FILE_SIZE);
        expect_eq!("file size matches stat", total_read, file_size_stat);
        println!(
            "  verified {} MB from {} in {} cycles",
            FILE_SIZE / (1024 * 1024),
            path,
            end - start
        );
    }

    // Now read file B again when it was loaded cold above after eviction.
    // This second read should be a cache hit (recently loaded).
    read_buf.clear();
    read_buf.set_len(FILE_SIZE);
    let (start_hot, _) = unsafe { tsc_read() };
    let fd_b = unsafe { sys_open_file(PATH_B, FD_FLAG_READ) };
    let mut hot_read = 0usize;
    if fd_b != usize::MAX {
        let n = unsafe { sys_read_file(fd_b, &mut read_buf) };
        if n != usize::MAX {
            hot_read = n;
        }
        unsafe { sys_close_file(fd_b) };
    }
    let (end_hot, _) = unsafe { tsc_read() };
    expect_eq!("hot re-read first chunk size", hot_read, CHUNK_SIZE);
    println!(
        "  hot re-read first chunk of B: {} cycles",
        end_hot - start_hot
    );

    unsafe { print_cache_stats("final") };

    for path in &PATHS {
        unsafe { sys_delete(path) };
    }
}

// Pin a file, then trigger eviction via evict_directory on its parent.
// Pinned file must remain in cache and readable.
unsafe fn suite_pin_survives_eviction() {
    suite_header("pin survives eviction");

    const DIR: &str = "/pintest";
    const PINNED: &str = "/pintest/pnd.txt";
    const UNPINNED: &str = "/pintest/unp.txt";
    const DATA: &[u8] = b"should stay in cache";
    const OTHER: &[u8] = b"can be evicted";

    unsafe { sys_delete(UNPINNED) };
    unsafe { sys_delete(PINNED) };
    unsafe { sys_delete(DIR) };
    unsafe { sys_create_dir(DIR) };
    unsafe { sys_create_file(PINNED) };
    unsafe { sys_create_file(UNPINNED) };

    let fd_p = unsafe { sys_open_file(PINNED, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd_p != usize::MAX {
        unsafe { sys_write_file(fd_p, 0, DATA) };
        unsafe { sys_close_file(fd_p) };
    }
    let fd_u = unsafe { sys_open_file(UNPINNED, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd_u != usize::MAX {
        unsafe { sys_write_file(fd_u, 0, OTHER) };
        unsafe { sys_close_file(fd_u) };
    }

    // Load both into cache
    let fd_pr = unsafe { sys_open_file(PINNED, FD_FLAG_READ) };
    if fd_pr != usize::MAX {
        let _ = unsafe { read_all(fd_pr, DATA.len()) };
        unsafe { sys_close_file(fd_pr) };
    }
    let fd_ur = unsafe { sys_open_file(UNPINNED, FD_FLAG_READ) };
    if fd_ur != usize::MAX {
        let _ = unsafe { read_all(fd_ur, OTHER.len()) };
        unsafe { sys_close_file(fd_ur) };
    }

    // Pin the file
    let pin_ok = unsafe { sys_pin_file(PINNED) };
    expect_true!("pin_file succeeds", pin_ok == 0);

    unsafe { print_cache_stats("before eviction") };

    // Evict the directory (will skip pinned entries)
    let _ = unsafe { sys_evict_directory(DIR) };

    unsafe { print_cache_stats("after eviction") };

    // Both must still be readable
    let fd_pv = unsafe { sys_open_file(PINNED, FD_FLAG_READ) };
    expect_true!("pinned file still readable", fd_pv != usize::MAX);
    if fd_pv != usize::MAX {
        let content = unsafe { read_all(fd_pv, DATA.len()) };
        unsafe { sys_close_file(fd_pv) };
        if let Some(data) = content {
            expect_eq!("pinned file size", data.len(), DATA.len());
            expect_true!("pinned file content intact", data.as_slice() == DATA);
        }
    }

    let fd_uv = unsafe { sys_open_file(UNPINNED, FD_FLAG_READ) };
    expect_true!(
        "unpinned file still readable from disk",
        fd_uv != usize::MAX
    );
    if fd_uv != usize::MAX {
        let content = unsafe { read_all(fd_uv, OTHER.len()) };
        unsafe { sys_close_file(fd_uv) };
        if let Some(data) = content {
            expect_true!("unpinned file content intact", data.as_slice() == OTHER);
        }
    }

    // Unpin and cleanup
    unsafe { sys_unpin_file(PINNED) };
    unsafe { sys_delete(UNPINNED) };
    unsafe { sys_delete(PINNED) };
    unsafe { sys_delete(DIR) };
}

// Write to a file, close it, reopen, write more data at a specific offset.
// Verifies the cache correctly reloads from disk and the final content is right.
unsafe fn suite_write_after_close() {
    suite_header("write after close and reopen");

    const PATH: &str = "/wac.txt";
    const FIRST: &[u8] = b"Hello, World!!!";
    const PATCH: &[u8] = b"MofuOS!";

    unsafe { sys_delete(PATH) };
    unsafe { sys_create_file(PATH) };

    let fd1 = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd1 == usize::MAX {
        println!("  SKIP (open 1 failed)");
        return;
    }
    let w1 = unsafe { sys_write_file(fd1, 0, FIRST) };
    expect_eq!("first write size", w1, FIRST.len());
    unsafe { sys_close_file(fd1) };
    unsafe { sys_flush_file_cache() };

    // Reopen, seek to offset 7, patch "MofuOS!" over "World!!"
    let fd2 = unsafe { sys_open_file(PATH, FD_FLAG_READ | FD_FLAG_WRITE) };
    if fd2 == usize::MAX {
        println!("  SKIP (open 2 failed)");
        return;
    }
    let mut skip = [0u8; 7];
    let _ = unsafe { sys_read_file(fd2, &mut skip) };
    let w2 = unsafe { sys_write_file(fd2, 7, PATCH) };
    expect_eq!("patch write size", w2, PATCH.len());
    unsafe { sys_close_file(fd2) };
    unsafe { sys_flush_file_cache() };

    // Read back: "Hello, MofuOS!" (first 7 from FIRST, then PATCH, same total length)
    let fd3 = unsafe { sys_open_file(PATH, FD_FLAG_READ) };
    expect_true!("open for verify", fd3 != usize::MAX);
    if fd3 != usize::MAX {
        let content = unsafe { read_all(fd3, 32) };
        unsafe { sys_close_file(fd3) };
        if let Some(data) = content {
            expect_eq!("patched file size", data.len(), FIRST.len());
            expect_true!("prefix intact", &data[..7] == &FIRST[..7]);
            expect_true!("patch correct", &data[7..7 + PATCH.len()] == PATCH);
        }
    }

    unsafe { sys_delete(PATH) };
}

unsafe fn suite_multi_file_flush() {
    suite_header("multi-file write then flush");

    // Open several files simultaneously, write to all, then flush in one call.
    const PATHS: &[&str] = &["/mfw0.txt", "/mfw1.txt", "/mfw2.txt", "/mfw3.txt"];
    const DATA: &[u8] = b"multi-file-write";
    const WRITE_ROUNDS: usize = 8;

    // Setup
    for path in PATHS {
        unsafe { sys_delete(path) };
        unsafe { sys_create_file(path) };
    }

    let mut fds = [usize::MAX; 4];
    for (i, path) in PATHS.iter().enumerate() {
        fds[i] = unsafe { sys_open_file(path, FD_FLAG_READ | FD_FLAG_WRITE) };
    }

    let (start_writes, _) = unsafe { tsc_read() };
    for _ in 0..WRITE_ROUNDS {
        for &fd in &fds {
            if fd != usize::MAX {
                unsafe { sys_write_file(fd, 0, DATA) };
            }
        }
    }
    let (end_writes, _) = unsafe { tsc_read() };
    let write_cycles = end_writes - start_writes;

    unsafe { print_cache_stats("before multi-flush") };

    let (start_flush, _) = unsafe { tsc_read() };
    let ok = unsafe { sys_flush_file_cache() };
    let (end_flush, _) = unsafe { tsc_read() };
    let flush_cycles = end_flush - start_flush;

    expect_true!("multi-file flush succeeds", ok);
    println!(
        "  [perf] {} files x {} writes: {} cycles",
        PATHS.len(),
        WRITE_ROUNDS,
        write_cycles
    );
    println!(
        "  [perf] flush {} files: {} cycles",
        PATHS.len(),
        flush_cycles
    );
    println!(
        "  [perf] write cycles/op: {}",
        write_cycles / (PATHS.len() * WRITE_ROUNDS) as u64
    );

    unsafe { print_cache_stats("after multi-flush") };

    // Verify sizes
    let expected = (DATA.len() * WRITE_ROUNDS) as u64;
    for path in PATHS {
        let mut stat = StatFlat::zeroed();
        unsafe { sys_stat_file(path, &mut stat) };
        expect_eq!("file size after flush", stat.size, expected);
    }

    for &fd in &fds {
        if fd != usize::MAX {
            unsafe { sys_close_file(fd) };
        }
    }
    for path in PATHS {
        unsafe { sys_delete(path) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    println!("cached_fs_test: starting");

    let mut cpu_info = CpuInfoFlat::zeroed();
    unsafe {
        sys_get_cpu_info(&mut cpu_info);
    }
    let freq_hz = cpu_info.tsc_frequency_hz;
    println!("CPU Frequency: {} MHz", freq_hz / 1_000_000);
    let cycles_to_us = |cycles: u64| -> u64 {
        if freq_hz > 0 {
            (cycles as u128 * 1_000_000 / freq_hz as u128) as u64
        } else {
            0
        }
    };

    unsafe {
        let (start_cycle, core) = rustspace::tsc_read();
        println!("Core: {}, Start Cycle: {}", core, start_cycle);

        suite_cache_basics();
        suite_repeated_reads();
        suite_directory_eviction();

        suite_stat_and_list();
        suite_create_write_read_delete();
        suite_sequential_reads();
        suite_directories();
        suite_error_cases();
        suite_overwrite();

        suite_write_throughput();
        suite_sequential_write_batching();
        suite_dirty_state();
        suite_multi_file_flush();

        ALLOCATOR.preallocate(2 * 1024 * 1024);

        suite_arena_grow_at_tail();
        suite_arena_grow_in_middle();
        suite_partial_write_preserves_prefix();

        #[cfg(feature = "test_big_files")]
        suite_eviction_under_pressure();

        suite_pin_survives_eviction();
        suite_write_after_close();

        let (end_cycle, _) = rustspace::tsc_read();
        let total_cycles = end_cycle - start_cycle;
        let total_us = cycles_to_us(total_cycles);
        println!("");
        println!("==========================================");
        println!(
            "  Total: {} cycles ({}us, {}ms)",
            total_cycles,
            total_us,
            total_us / 1000
        );
        println!("==========================================");

        print_cache_stats("final");
    }

    print_summary();
    println!("cached_fs_test: done");

    unsafe { sys_exit(0) }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("cached_fs_test: PANIC");
    unsafe { sys_exit(1) }
}
