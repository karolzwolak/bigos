#![no_std]
#![no_main]

use core::sync::atomic::Ordering;
use rustspace::{
    DirEntryFlat, FD_FLAG_READ, FD_FLAG_WRITE, File, MAX_OPEN_FILES, SYS_CREATE_DIR, SYS_LIST_DIR,
    SYS_STAT_FILE, StatFlat, SyscallError, program_shared_data, sys_close_file, sys_create_dir,
    sys_create_file, sys_delete, sys_exit, sys_list_dir, sys_open_file, sys_read_file_at,
    sys_stat_file, syscall3, syscall6,
};

// Indexes into the shared test counters, the kernel tests use the same values
const COUNTER_RUNNING: usize = 0;
const COUNTER_CHECKS_PASSED: usize = 1;
const COUNTER_CHECKS_FAILED: usize = 2;
const COUNTER_LEAKED_FDS: usize = 3;

const EXIT_CODE_PANIC: i32 = 255;
const EXIT_CODE_CHECKS_FAILED: i32 = 1;

// must match TEST_DISK_PATTERN_BYTES in xtask and the generator in create_disk_image.sh
const PATTERN_PERIOD: usize = 251;
const BIG_FILE_SIZE_BYTES: usize = 100_000;

const READ_WRITE: u8 = FD_FLAG_READ | FD_FLAG_WRITE;
const HELLO_PATH: &str = "/HELLO.TXT";
const BIG_PATH: &str = "/BIG.BIN";
const DIRECTORY_PATH: &str = "/DIR1";
const SMALL_PATH: &str = "/FSTEST.TXT";
const SEQUENTIAL_PATH: &str = "/FSSEQ.BIN";
const MULTI_CLUSTER_PATH: &str = "/FSBIG.BIN";
const OVERWRITE_PATH: &str = "/FSOVER.BIN";
const ERRORS_PATH: &str = "/FSERR.TXT";
const MISSING_PATH: &str = "/NOPE.TXT";
const BAD_NAME_PATH: &str = "/TOOLONGNAME1.TXT";

const HELLO_SIZE_BYTES: u64 = 5;
const NEW_DIR_PATH: &str = "/FSDIR";
const NEW_DIR_FILE_PATH: &str = "/FSDIR/IN.TXT";
const PAGED_DIR_PATH: &str = "/FSPAGE";
const PAGED_DIR_PREFIX: &str = "/FSPAGE/F";
const PAGED_FILE_COUNT: usize = 10;
const PAGE_ENTRIES: usize = 4;
const LIST_BUFFER_ENTRIES: usize = 16;
const NESTED_DIR_PATH: &str = "/DIR1/DIR2";

const NULL_POINTER: u64 = 0;
const KERNEL_SPACE_POINTER: u64 = 0xFFFF_8000_0000_1000;
const UNMAPPED_USER_POINTER: u64 = 0x0000_1000_0000_0000;
const STRADDLING_USER_POINTER: u64 = 0x0000_7FFF_FFFF_FF80;
// within MAX_PATH_LEN, so the range check is what rejects it
const STRADDLING_LENGTH: u64 = 0x100;

const SEQUENTIAL_FILE_BYTES: usize = 2000;
const SEQUENTIAL_CHUNK_BYTES: usize = 300;
const OVERWRITE_FILE_BYTES: usize = 10_000;
const OVERWRITE_START_BYTES: usize = 3000;
const OVERWRITE_LENGTH_BYTES: usize = 1500;
const OVERWRITE_BYTE: u8 = 0xEE;
const APPEND_BYTES: usize = 100;
const REUSED_FD: usize = 5;
const UNUSED_FD_NUMBER: usize = 1_000_000;
const NEVER_OPENED_FD: usize = MAX_OPEN_FILES - 2;
const BAD_FLAGS: u8 = 0x80;
const LEAKED_FD_COUNT: u32 = 3;

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
        "call {fs_main}",
        "ud2",
        fs_main = sym fs_main,
    )
}

fn check(name: &str, condition: bool) {
    let counters = &program_shared_data().test_counters;
    if condition {
        counters[COUNTER_CHECKS_PASSED].fetch_add(1, Ordering::AcqRel);
        return;
    }

    counters[COUNTER_CHECKS_FAILED].fetch_add(1, Ordering::AcqRel);
    rustspace::println!("fs_test: FAILED: {}", name);
}

fn expect_error<T>(name: &str, result: Result<T, SyscallError>, expected: SyscallError) {
    check(name, matches!(result, Err(error) if error == expected));
}

fn pattern_byte(index: usize) -> u8 {
    (index % PATTERN_PERIOD) as u8
}

fn fill_pattern(buffer: &mut [u8], start_index: usize) {
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = pattern_byte(start_index + index);
    }
}

fn matches_pattern(buffer: &[u8], start_index: usize) -> bool {
    buffer
        .iter()
        .enumerate()
        .all(|(index, byte)| *byte == pattern_byte(start_index + index))
}

fn create_with_pattern(path: &str, length: usize, scratch: &mut [u8]) -> bool {
    debug_assert!(length <= scratch.len());

    if sys_create_file(path).is_err() {
        return false;
    }
    let Ok(file) = File::open(path, READ_WRITE) else {
        return false;
    };

    fill_pattern(&mut scratch[..length], 0);

    file.write_at(0, &scratch[..length]) == Ok(length)
}

fn test_create_write_read_delete() {
    const CONTENT: &[u8] = b"hello fs";

    check("create small file", sys_create_file(SMALL_PATH).is_ok());

    let file = File::open(SMALL_PATH, READ_WRITE).expect("open of a created file");
    check(
        "write small file",
        file.write_at(0, CONTENT) == Ok(CONTENT.len()),
    );

    let mut read_back = [0u8; CONTENT.len() + 8];
    let read = file.read_at(0, &mut read_back);
    check("read back length", read == Ok(CONTENT.len()));
    check("read back content", &read_back[..CONTENT.len()] == CONTENT);
    check(
        "read at the end",
        file.read_at(CONTENT.len(), &mut read_back) == Ok(0),
    );

    drop(file);
    check("delete small file", sys_delete(SMALL_PATH).is_ok());
    expect_error(
        "open of a deleted file",
        sys_open_file(SMALL_PATH, FD_FLAG_READ),
        SyscallError::NOT_FOUND,
    );
}

fn test_sequential_reads() {
    let mut scratch = [0u8; SEQUENTIAL_FILE_BYTES];
    check(
        "create sequential file",
        create_with_pattern(SEQUENTIAL_PATH, SEQUENTIAL_FILE_BYTES, &mut scratch),
    );

    let mut file = File::open(SEQUENTIAL_PATH, FD_FLAG_READ).expect("open of a created file");
    let mut chunk = [0u8; SEQUENTIAL_CHUNK_BYTES];
    let mut expected_offset = 0;
    let mut all_chunks_ok = true;

    loop {
        let read = file.read(&mut chunk).expect("read of an open file");
        if read == 0 {
            break;
        }

        all_chunks_ok &= matches_pattern(&chunk[..read], expected_offset);
        expected_offset += read;
        all_chunks_ok &= file.offset_bytes == expected_offset;
    }

    check("sequential chunks match the pattern", all_chunks_ok);
    check(
        "sequential reads cover the file",
        expected_offset == SEQUENTIAL_FILE_BYTES,
    );
    check("read at the end returns 0", file.read(&mut chunk) == Ok(0));

    drop(file);
    check(
        "delete sequential file",
        sys_delete(SEQUENTIAL_PATH).is_ok(),
    );
}

fn test_multi_cluster() {
    let mut write_buffer = [0u8; BIG_FILE_SIZE_BYTES];
    check(
        "write the multi cluster file in one call",
        create_with_pattern(MULTI_CLUSTER_PATH, BIG_FILE_SIZE_BYTES, &mut write_buffer),
    );

    let mut read_buffer = [0u8; BIG_FILE_SIZE_BYTES];
    let file = File::open(MULTI_CLUSTER_PATH, FD_FLAG_READ).expect("open of a created file");
    let read = file.read_at(0, &mut read_buffer);
    check("multi cluster read length", read == Ok(BIG_FILE_SIZE_BYTES));
    check("multi cluster read content", read_buffer == write_buffer);

    let mut tail = [0u8; 10];
    check(
        "read crossing the end returns the rest",
        file.read_at(BIG_FILE_SIZE_BYTES - 1, &mut tail) == Ok(1),
    );
    check(
        "read at the end returns 0",
        file.read_at(BIG_FILE_SIZE_BYTES, &mut tail) == Ok(0),
    );
    drop(file);

    // the template file holds the same pattern, written by the image generator
    read_buffer.fill(0);
    let template = File::open(BIG_PATH, FD_FLAG_READ).expect("open of the template file");
    let read = template.read_at(0, &mut read_buffer);
    check("template read length", read == Ok(BIG_FILE_SIZE_BYTES));
    check("template content", matches_pattern(&read_buffer, 0));
    drop(template);

    check(
        "delete multi cluster file",
        sys_delete(MULTI_CLUSTER_PATH).is_ok(),
    );
}

fn test_overwrite_middle() {
    let mut scratch = [0u8; OVERWRITE_FILE_BYTES + APPEND_BYTES];
    check(
        "create overwrite file",
        create_with_pattern(OVERWRITE_PATH, OVERWRITE_FILE_BYTES, &mut scratch),
    );

    let file = File::open(OVERWRITE_PATH, READ_WRITE).expect("open of a created file");
    let replacement = [OVERWRITE_BYTE; OVERWRITE_LENGTH_BYTES];
    check(
        "overwrite in the middle",
        file.write_at(OVERWRITE_START_BYTES, &replacement) == Ok(OVERWRITE_LENGTH_BYTES),
    );

    let mut read_back = [0u8; OVERWRITE_FILE_BYTES];
    check(
        "read back after overwrite",
        file.read_at(0, &mut read_back) == Ok(OVERWRITE_FILE_BYTES),
    );

    let overwrite_end = OVERWRITE_START_BYTES + OVERWRITE_LENGTH_BYTES;
    check(
        "bytes before the overwrite are kept",
        matches_pattern(&read_back[..OVERWRITE_START_BYTES], 0),
    );
    check(
        "overwritten range has the new bytes",
        read_back[OVERWRITE_START_BYTES..overwrite_end]
            .iter()
            .all(|byte| *byte == OVERWRITE_BYTE),
    );
    check(
        "bytes after the overwrite are kept",
        matches_pattern(&read_back[overwrite_end..], overwrite_end),
    );

    let append = [OVERWRITE_BYTE; APPEND_BYTES];
    check(
        "append at the end",
        file.write_at(OVERWRITE_FILE_BYTES, &append) == Ok(APPEND_BYTES),
    );
    check(
        "append grew the file",
        file.read_at(OVERWRITE_FILE_BYTES, &mut scratch) == Ok(APPEND_BYTES),
    );

    drop(file);
    check("delete overwrite file", sys_delete(OVERWRITE_PATH).is_ok());
}

fn test_error_cases() {
    expect_error(
        "open of a missing file",
        sys_open_file(MISSING_PATH, FD_FLAG_READ),
        SyscallError::NOT_FOUND,
    );
    expect_error(
        "open with no flags",
        sys_open_file(HELLO_PATH, 0),
        SyscallError::INVALID_ARGUMENT,
    );
    expect_error(
        "open with unknown flags",
        sys_open_file(HELLO_PATH, BAD_FLAGS),
        SyscallError::INVALID_ARGUMENT,
    );
    expect_error(
        "open of a directory",
        sys_open_file(DIRECTORY_PATH, FD_FLAG_READ),
        SyscallError::IS_A_DIRECTORY,
    );
    expect_error(
        "open of an empty path",
        sys_open_file("", FD_FLAG_READ),
        SyscallError::INVALID_PATH,
    );
    expect_error(
        "delete of a missing file",
        sys_delete(MISSING_PATH),
        SyscallError::NOT_FOUND,
    );
    expect_error(
        "create with an invalid name",
        sys_create_file(BAD_NAME_PATH),
        SyscallError::INVALID_PATH,
    );

    check(
        "create the error test file",
        sys_create_file(ERRORS_PATH).is_ok(),
    );
    expect_error(
        "create of an existing file",
        sys_create_file(ERRORS_PATH),
        SyscallError::ALREADY_EXISTS,
    );

    let write_only = File::open(ERRORS_PATH, FD_FLAG_WRITE).expect("open of a created file");
    let mut buffer = [0u8; 16];
    expect_error(
        "read on a write only fd",
        write_only.read_at(0, &mut buffer),
        SyscallError::PERMISSION_DENIED,
    );
    expect_error(
        "write past the end of the file",
        write_only.write_at(1, &buffer),
        SyscallError::INVALID_ARGUMENT,
    );

    let read_only = File::open(HELLO_PATH, FD_FLAG_READ).expect("open of the template file");
    expect_error(
        "write on a read only fd",
        read_only.write_at(0, &buffer),
        SyscallError::PERMISSION_DENIED,
    );

    let closed_fd = read_only.fd;
    drop(read_only);
    expect_error(
        "read on a closed fd",
        sys_read_file_at(closed_fd, 0, &mut buffer),
        SyscallError::INVALID_FD,
    );
    expect_error(
        "close of a closed fd",
        sys_close_file(closed_fd),
        SyscallError::INVALID_FD,
    );
    expect_error(
        "read on an fd that was never opened",
        sys_read_file_at(NEVER_OPENED_FD, 0, &mut buffer),
        SyscallError::INVALID_FD,
    );
    expect_error(
        "read on an fd number out of range",
        sys_read_file_at(UNUSED_FD_NUMBER, 0, &mut buffer),
        SyscallError::INVALID_FD,
    );

    drop(write_only);
    check(
        "delete the error test file",
        sys_delete(ERRORS_PATH).is_ok(),
    );
}

fn contains_name(entries: &[DirEntryFlat], name: &str) -> bool {
    entries.iter().any(|entry| entry.name_str() == name)
}

fn list_all(path: &str, entries: &mut [DirEntryFlat]) -> Result<usize, SyscallError> {
    sys_list_dir(path, 0, entries)
}

fn test_stat() {
    let mut stat = StatFlat::zeroed();

    check(
        "stat HELLO.TXT",
        sys_stat_file(HELLO_PATH, &mut stat).is_ok(),
    );
    check("stat name", stat.name_str() == "HELLO.TXT");
    check("stat file flag", stat.is_dir == 0);
    check("stat file size", stat.size == HELLO_SIZE_BYTES);

    let mut stat = StatFlat::zeroed();
    check("stat big file", sys_stat_file(BIG_PATH, &mut stat).is_ok());
    check("stat big size", stat.size == BIG_FILE_SIZE_BYTES as u64);

    let mut stat = StatFlat::zeroed();
    check(
        "stat directory",
        sys_stat_file(DIRECTORY_PATH, &mut stat).is_ok(),
    );
    check("stat directory flag", stat.is_dir == 1);
    check("stat directory name", stat.name_str() == "DIR1");

    let mut stat = StatFlat::zeroed();
    check(
        "stat nested file",
        sys_stat_file("/DIR1/DIR2/F2_1.TXT", &mut stat).is_ok(),
    );
    check("stat nested name", stat.name_str() == "F2_1.TXT");

    expect_error(
        "stat of a missing file",
        sys_stat_file(MISSING_PATH, &mut stat),
        SyscallError::NOT_FOUND,
    );
}

fn test_list_directories() {
    let mut entries = [DirEntryFlat::zeroed(); LIST_BUFFER_ENTRIES];

    let count = list_all("/", &mut entries).expect("list of the root");
    let root = &entries[..count];
    check(
        "root lists the template entries",
        ["HELLO.TXT", "EMPTY.TXT", "BIG.BIN", "DIR1"]
            .iter()
            .all(|name| contains_name(root, name)),
    );
    let hello_entry_ok = root
        .iter()
        .any(|entry| entry.name_str() == "HELLO.TXT" && entry.size == HELLO_SIZE_BYTES);
    check("root entry carries the size", hello_entry_ok);
    let directory_entry_ok = root
        .iter()
        .any(|entry| entry.name_str() == "DIR1" && entry.is_dir == 1);
    check("root entry carries the directory flag", directory_entry_ok);

    let count = list_all(DIRECTORY_PATH, &mut entries).expect("list of DIR1");
    check("nested list count", count == 3);
    check(
        "nested list names",
        ["F1_1.TXT", "F1_2.TXT", "DIR2"]
            .iter()
            .all(|name| contains_name(&entries[..count], name)),
    );

    let count = list_all(NESTED_DIR_PATH, &mut entries).expect("list of DIR1/DIR2");
    check("second level list count", count == 2);
    check(
        "second level list names",
        contains_name(&entries[..count], "F2_1.TXT")
            && contains_name(&entries[..count], "F2_2.TXT"),
    );

    expect_error(
        "list of a file",
        list_all(HELLO_PATH, &mut entries),
        SyscallError::NOT_A_DIRECTORY,
    );
    expect_error(
        "list of a missing directory",
        list_all("/NODIR", &mut entries),
        SyscallError::NOT_FOUND,
    );
    check(
        "list into an empty buffer",
        list_all("/", &mut entries[..0]) == Ok(0),
    );
}

fn test_directory_lifecycle() {
    check("create directory", sys_create_dir(NEW_DIR_PATH).is_ok());
    expect_error(
        "create of an existing directory",
        sys_create_dir(NEW_DIR_PATH),
        SyscallError::ALREADY_EXISTS,
    );
    expect_error(
        "create directory in a missing parent",
        sys_create_dir("/NODIR/SUB"),
        SyscallError::NOT_FOUND,
    );

    let mut stat = StatFlat::zeroed();
    check(
        "stat created directory",
        sys_stat_file(NEW_DIR_PATH, &mut stat).is_ok(),
    );
    check("created directory flag", stat.is_dir == 1);

    let mut entries = [DirEntryFlat::zeroed(); LIST_BUFFER_ENTRIES];
    check(
        "new directory lists empty",
        list_all(NEW_DIR_PATH, &mut entries) == Ok(0),
    );

    check(
        "create file in directory",
        sys_create_file(NEW_DIR_FILE_PATH).is_ok(),
    );
    let count = list_all(NEW_DIR_PATH, &mut entries).expect("list of the new directory");
    check(
        "directory lists the new file",
        count == 1 && entries[0].name_str() == "IN.TXT",
    );

    expect_error(
        "delete of a non empty directory",
        sys_delete(NEW_DIR_PATH),
        SyscallError::DIRECTORY_NOT_EMPTY,
    );

    check(
        "delete file in directory",
        sys_delete(NEW_DIR_FILE_PATH).is_ok(),
    );
    check(
        "delete the empty directory",
        sys_delete(NEW_DIR_PATH).is_ok(),
    );
    expect_error(
        "stat of a deleted directory",
        sys_stat_file(NEW_DIR_PATH, &mut stat),
        SyscallError::NOT_FOUND,
    );
}

fn paged_file_path(index: usize, buffer: &mut [u8; 32]) -> &str {
    let prefix = PAGED_DIR_PREFIX.as_bytes();
    buffer[..prefix.len()].copy_from_slice(prefix);
    buffer[prefix.len()] = b'0' + index as u8;

    core::str::from_utf8(&buffer[..prefix.len() + 1]).unwrap_or("")
}

fn test_list_paging() {
    check(
        "create paged directory",
        sys_create_dir(PAGED_DIR_PATH).is_ok(),
    );

    let mut path_buffer = [0u8; 32];
    for index in 0..PAGED_FILE_COUNT {
        let path = paged_file_path(index, &mut path_buffer);
        check("create paged file", sys_create_file(path).is_ok());
    }

    let mut page = [DirEntryFlat::zeroed(); PAGE_ENTRIES];
    let mut seen = [false; PAGED_FILE_COUNT];
    let mut start_index = 0;
    let mut page_sizes_ok = true;
    let mut page_count = 0;

    loop {
        let count = sys_list_dir(PAGED_DIR_PATH, start_index, &mut page).expect("paged list");
        if count == 0 {
            break;
        }

        page_count += 1;
        page_sizes_ok &= count <= PAGE_ENTRIES;
        for entry in &page[..count] {
            let digit = entry.name.get(1).copied().unwrap_or(0);
            let index = digit.wrapping_sub(b'0') as usize;
            if index < PAGED_FILE_COUNT {
                seen[index] = true;
            }
        }

        start_index += count;
    }

    check("pages never exceed the buffer", page_sizes_ok);
    check("paging used three pages", page_count == 3);
    check(
        "paging covers every entry once",
        start_index == PAGED_FILE_COUNT,
    );
    check(
        "paging sees every file",
        seen.iter().all(|present| *present),
    );
    check(
        "start index past the end lists nothing",
        sys_list_dir(PAGED_DIR_PATH, PAGED_FILE_COUNT + 5, &mut page) == Ok(0),
    );

    for index in 0..PAGED_FILE_COUNT {
        let path = paged_file_path(index, &mut path_buffer);
        check("delete paged file", sys_delete(path).is_ok());
    }
    check("delete paged directory", sys_delete(PAGED_DIR_PATH).is_ok());
}

fn invalid_ptr_result(ret: u64) -> bool {
    ret == SyscallError::INVALID_PTR.0.wrapping_neg()
}

fn test_invalid_pointers() {
    let path_len = HELLO_PATH.len() as u64;
    let path_ptr = HELLO_PATH.as_ptr() as u64;
    let mut entries = [DirEntryFlat::zeroed(); PAGE_ENTRIES];
    let entries_ptr = entries.as_mut_ptr() as u64;
    let entries_bytes = core::mem::size_of_val(&entries) as u64;
    let bad_pointers = [
        NULL_POINTER,
        KERNEL_SPACE_POINTER,
        UNMAPPED_USER_POINTER,
        STRADDLING_USER_POINTER,
    ];

    for bad_pointer in bad_pointers {
        let length = if bad_pointer == STRADDLING_USER_POINTER {
            STRADDLING_LENGTH
        } else {
            path_len
        };

        // SAFETY: the kernel validates every pointer, none of these is dereferenced here
        unsafe {
            check(
                "stat with a bad path pointer",
                invalid_ptr_result(syscall3(SYS_STAT_FILE, bad_pointer, length, path_ptr)),
            );
            check(
                "create dir with a bad path pointer",
                invalid_ptr_result(syscall3(SYS_CREATE_DIR, bad_pointer, length, 0)),
            );
            check(
                "list with a bad path pointer",
                invalid_ptr_result(syscall6(
                    SYS_LIST_DIR,
                    bad_pointer,
                    length,
                    entries_ptr,
                    entries_bytes,
                    0,
                    0,
                )),
            );
            check(
                "stat with a bad out pointer",
                invalid_ptr_result(syscall3(SYS_STAT_FILE, path_ptr, path_len, bad_pointer)),
            );
            check(
                "list with a bad out pointer",
                invalid_ptr_result(syscall6(
                    SYS_LIST_DIR,
                    path_ptr,
                    path_len,
                    bad_pointer,
                    entries_bytes,
                    0,
                    0,
                )),
            );
        }
    }

    // the out buffer starts in user space but runs into kernel space
    // SAFETY: as above
    let straddling_list = unsafe {
        syscall6(
            SYS_LIST_DIR,
            path_ptr,
            path_len,
            STRADDLING_USER_POINTER,
            STRADDLING_LENGTH,
            0,
            0,
        )
    };
    check(
        "list with an out buffer past user space",
        invalid_ptr_result(straddling_list),
    );
}

fn test_fd_table() {
    let mut fds = [0usize; MAX_OPEN_FILES];
    let mut lowest_slot_first = true;
    for (index, slot) in fds.iter_mut().enumerate() {
        match sys_open_file(HELLO_PATH, FD_FLAG_READ) {
            Ok(fd) => {
                *slot = fd;
                lowest_slot_first &= fd == index;
            }
            Err(_) => lowest_slot_first = false,
        }
    }
    check("fds are handed out lowest slot first", lowest_slot_first);

    expect_error(
        "open with a full fd table",
        sys_open_file(HELLO_PATH, FD_FLAG_READ),
        SyscallError::TOO_MANY_OPEN_FILES,
    );

    // closing one fd must not renumber any other open fd
    check("close one fd", sys_close_file(fds[REUSED_FD]).is_ok());
    let mut byte = [0u8; 1];
    let neighbours_ok = [REUSED_FD - 1, REUSED_FD + 1]
        .iter()
        .all(|neighbour| sys_read_file_at(fds[*neighbour], 0, &mut byte) == Ok(1));
    check("other fds survive a close", neighbours_ok);
    check(
        "the closed slot is reused",
        sys_open_file(HELLO_PATH, FD_FLAG_READ) == Ok(REUSED_FD),
    );

    for fd in fds {
        check("close every fd", sys_close_file(fd).is_ok());
    }
}

/// Leaves fds open at exit, the kernel test checks that teardown releases them
fn leak_open_fds() {
    for _ in 0..LEAKED_FD_COUNT {
        let file = File::open(HELLO_PATH, FD_FLAG_READ).expect("open of the template file");
        core::mem::forget(file);
    }

    program_shared_data().test_counters[COUNTER_LEAKED_FDS]
        .store(LEAKED_FD_COUNT, Ordering::Release);
}

extern "C" fn fs_main() -> ! {
    let counters = &program_shared_data().test_counters;
    counters[COUNTER_RUNNING].fetch_add(1, Ordering::AcqRel);

    test_create_write_read_delete();
    test_sequential_reads();
    test_multi_cluster();
    test_overwrite_middle();
    test_error_cases();
    test_stat();
    test_list_directories();
    test_directory_lifecycle();
    test_list_paging();
    test_invalid_pointers();
    test_fd_table();
    leak_open_fds();

    if counters[COUNTER_CHECKS_FAILED].load(Ordering::Acquire) != 0 {
        sys_exit(EXIT_CODE_CHECKS_FAILED)
    }

    sys_exit(0)
}
