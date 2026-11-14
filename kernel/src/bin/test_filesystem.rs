#![no_std]
#![no_main]

extern crate alloc;
extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    filesystem::{fat32::test_data::create_fat32_image, get_vfs, init_filesystem},
    io::disk::{BlockDevice, RamDisk},
    testing::{test_case, test_panic_handler},
};

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    unsafe { kernel::boot_common::bsp_early_init() };

    let image = create_fat32_image();
    init_filesystem(BlockDevice::Ram(RamDisk::from_image(&*image)))
        .expect("filesystem init failed");

    kernel::testing::run_all_tests()
}

#[test_case]
fn test_init_and_list() {
    let entries = get_vfs()
        .list_directory("/")
        .expect("list_directory failed");
    assert!(!entries.is_empty(), "root directory is empty");
    let names: alloc::vec::Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"HELLO.TXT") || names.iter().any(|n| n.to_uppercase() == "HELLO.TXT"),
        "HELLO.TXT not found in {:?}",
        names
    );
}

#[test_case]
fn test_read_file() {
    let mut buf = [0u8; 64];
    let read = get_vfs()
        .read_file("HELLO.TXT", 0, &mut buf)
        .expect("read_file failed");
    assert!(read > 0, "read 0 bytes");
    let text = core::str::from_utf8(&buf[..read]).expect("file content not UTF-8");
    assert!(text.contains("BigOS"), "unexpected content: {}", text);
}

#[test_case]
fn test_create_and_delete() {
    {
        let mut vfs = get_vfs();
        vfs.create_file("/newfile.txt").expect("create_file failed");
        vfs.create_directory("/newdir")
            .expect("create_directory failed");
    }
    {
        let mut vfs = get_vfs();
        vfs.delete("/newfile.txt")
            .expect("delete newfile.txt failed");
        vfs.delete("/newdir").expect("delete newdir failed");
    }
    let entries = get_vfs()
        .list_directory("/")
        .expect("list after delete failed");
    let names: alloc::vec::Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names
            .iter()
            .all(|name| name.to_uppercase() != "NEWFILE.TXT"),
        "newfile.txt still present after delete"
    );
}

const PATTERN_PERIOD: usize = 251;
const WRITE_TEST_SIZE_BYTES: usize = 9000;

fn fill_pattern(buffer: &mut [u8], first_index: usize) {
    for (i, byte) in buffer.iter_mut().enumerate() {
        *byte = ((first_index + i) % PATTERN_PERIOD) as u8;
    }
}

#[test_case]
fn test_write_grows_across_clusters() {
    let mut vfs = get_vfs();
    vfs.create_file("/GROW.BIN").expect("create_file failed");

    let mut data = [0u8; WRITE_TEST_SIZE_BYTES];
    fill_pattern(&mut data, 0);
    let written = vfs
        .write_file("GROW.BIN", 0, &data)
        .expect("write_file failed");
    assert_eq!(written, WRITE_TEST_SIZE_BYTES);
    assert_eq!(
        vfs.resolve_path("GROW.BIN").expect("resolve failed").size,
        WRITE_TEST_SIZE_BYTES
    );

    let mut read_back = [0u8; WRITE_TEST_SIZE_BYTES + 16];
    let read = vfs
        .read_file("GROW.BIN", 0, &mut read_back)
        .expect("read_file failed");
    assert_eq!(read, WRITE_TEST_SIZE_BYTES);
    assert_eq!(&read_back[..read], &data[..]);

    // overwrite in the middle across a cluster boundary, neighbours stay intact
    let patch = [0xEEu8; 300];
    let patch_offset = 4000;
    vfs.write_file("GROW.BIN", patch_offset, &patch)
        .expect("overwrite failed");
    let read = vfs
        .read_file("GROW.BIN", 0, &mut read_back)
        .expect("read_file failed");
    assert_eq!(read, WRITE_TEST_SIZE_BYTES);
    assert_eq!(&read_back[..patch_offset], &data[..patch_offset]);
    assert_eq!(&read_back[patch_offset..patch_offset + patch.len()], &patch);
    assert_eq!(
        &read_back[patch_offset + patch.len()..read],
        &data[patch_offset + patch.len()..]
    );

    vfs.delete("GROW.BIN").expect("delete failed");
}

#[test_case]
fn test_append_and_eof() {
    let mut vfs = get_vfs();
    vfs.create_file("/APPEND.TXT").expect("create_file failed");

    let mut offset = 0;
    let mut chunk = [0u8; 700];
    for _ in 0..10 {
        fill_pattern(&mut chunk, offset);
        let written = vfs
            .write_file("APPEND.TXT", offset, &chunk)
            .expect("append failed");
        assert_eq!(written, chunk.len());
        offset += written;
    }

    let mut read_back = [0u8; 64];
    assert_eq!(
        vfs.read_file("APPEND.TXT", offset, &mut read_back).unwrap(),
        0
    );
    // crossing EOF returns only the remaining bytes
    let read = vfs
        .read_file("APPEND.TXT", offset - 10, &mut read_back)
        .unwrap();
    assert_eq!(read, 10);
    let mut expected = [0u8; 10];
    fill_pattern(&mut expected, offset - 10);
    assert_eq!(&read_back[..read], &expected);

    assert!(vfs.write_file("APPEND.TXT", offset + 1, &chunk).is_err());
    vfs.delete("APPEND.TXT").expect("delete failed");
}

#[test_case]
fn test_empty_files_have_distinct_handles() {
    let mut vfs = get_vfs();
    let first = vfs.create_file("/EMPTYA.TXT").unwrap();
    let second = vfs.create_file("/EMPTYB.TXT").unwrap();
    assert_ne!(first.node_id, second.node_id);

    let mut buffer = [0u8; 8];
    assert_eq!(vfs.read_file("EMPTYA.TXT", 0, &mut buffer).unwrap(), 0);

    // writing allocates the first cluster, the old handle keeps working
    vfs.write_file("EMPTYB.TXT", 0, b"abc").unwrap();
    assert_eq!(vfs.driver.get_node(second.node_id).unwrap().size, 3);
    assert_eq!(vfs.driver.get_node(first.node_id).unwrap().size, 0);

    vfs.delete("EMPTYA.TXT").unwrap();
    vfs.delete("EMPTYB.TXT").unwrap();
}
