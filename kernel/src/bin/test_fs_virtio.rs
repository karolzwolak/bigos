#![no_std]
#![no_main]

extern crate alloc;
extern crate kernel;

use alloc::{format, vec, vec::Vec};
use core::mem::replace;
use core::panic::PanicInfo;
use kernel::{
    filesystem::{
        FileType, Vfs,
        fat32::{Fat32Driver, ROOT_CLUSTER},
        get_vfs, init_filesystem,
        vfs::FileSystemError,
    },
    io::{
        disk::{BlockDevice, RamDisk, SECTOR_SIZE_BYTES},
        virtio::VirtioBlk,
    },
    testing::{test_case, test_panic_handler},
};

const EXPECTED_BYTES_PER_SECTOR: u16 = 512;
const EXPECTED_NUM_FATS: u8 = 2;
const EXPECTED_ROOT_CLUSTER: u32 = 2;
const FAT_ENTRY_SIZE_BYTES: usize = 4;
const FAT_ENTRY_MASK: u32 = 0x0FFF_FFFF;
const DIRECTORY_ENTRY_SIZE_BYTES: usize = 32;

// must match TEST_DISK_PATTERN_BYTES in xtask and the generator in create_disk_image.sh
const BIG_FILE_SIZE_BYTES: usize = 100_000;
const PATTERN_PERIOD: usize = 251;
const HELLO_CONTENT: &[u8] = b"hello";
const LFN_DIRECTORY_PATH: &str = "/LFNDIR";
const LFN_CONTENT: &[u8] = b"lfn";
const NESTED_FILE_PATH_1: &str = "/DIR1/F1_1.TXT";
const NESTED_FILE_PATH_2: &str = "/DIR1/DIR2/F2_1.TXT";

const OVERWRITE_BYTE: u8 = 0xEE;
const OVERWRITE_START_CLUSTERS: usize = 7;
const OVERWRITE_START_BACKOFF_BYTES: usize = 13;
const OVERWRITE_LENGTH_CLUSTERS: usize = 3;
const OVERWRITE_LENGTH_EXTRA_BYTES: usize = 29;
const GROW_CLUSTERS: usize = 6;
const GROW_EXTRA_BYTES: usize = 17;
const APPEND_CHUNK_BYTES: usize = 37;
const APPEND_CLUSTERS: usize = 4;
const EXTRA_DIRECTORY_ENTRIES: usize = 3;
const PAST_EOF_GAP_BYTES: usize = 1;
const EOF_CROSSING_TAIL_BYTES: usize = 10;
const EOF_CROSSING_REQUEST_BYTES: usize = 100;
const REMOUNT_CLUSTERS: usize = 5;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    unsafe { kernel::boot_common::bsp_early_init() };

    let device = VirtioBlk::find_and_init().expect("virtio-blk device not found");
    init_filesystem(BlockDevice::VirtioBlk(device)).expect("mounting the test disk failed");

    kernel::testing::run_all_tests()
}

fn pattern_byte(index: usize) -> u8 {
    (index % PATTERN_PERIOD) as u8
}

fn pattern_buffer(start_index: usize, length: usize) -> Vec<u8> {
    (start_index..start_index + length)
        .map(pattern_byte)
        .collect()
}

fn cluster_size_bytes(vfs: &Vfs) -> usize {
    vfs.driver.cluster_size
}

fn read_exact(vfs: &mut Vfs, path: &str, offset: usize, length: usize) -> Vec<u8> {
    let mut buffer = vec![0u8; length];
    let read = vfs
        .read_file(path, offset, &mut buffer)
        .expect("read_file failed");
    assert_eq!(read, length, "short read of {} at {}", path, offset);

    buffer
}

fn file_size(vfs: &mut Vfs, path: &str) -> usize {
    vfs.resolve_path(path).expect("resolve_path failed").size
}

fn write_all(vfs: &mut Vfs, path: &str, offset: usize, data: &[u8]) {
    let written = vfs
        .write_file(path, offset, data)
        .expect("write_file failed");
    assert_eq!(written, data.len(), "short write to {}", path);
}

fn list_names(vfs: &mut Vfs, path: &str) -> Vec<alloc::string::String> {
    vfs.list_directory(path)
        .expect("list_directory failed")
        .into_iter()
        .map(|node| node.name)
        .collect()
}

fn read_fat_copy(vfs: &mut Vfs, fat_index: u64) -> Vec<u8> {
    let driver = &mut vfs.driver;
    let fat_size_sectors = driver.boot_sector.fat_size_32 as u64;
    let mut fat = vec![0u8; fat_size_sectors as usize * SECTOR_SIZE_BYTES];
    driver
        .device
        .read_sectors(
            driver.fat_start_sector + fat_index * fat_size_sectors,
            &mut fat,
        )
        .expect("reading a FAT copy failed");

    fat
}

fn count_free_clusters(vfs: &mut Vfs) -> usize {
    let fat = read_fat_copy(vfs, 0);
    let max_cluster = vfs.driver.max_cluster as usize;

    (ROOT_CLUSTER as usize..max_cluster)
        .filter(|&cluster| {
            let offset = cluster * FAT_ENTRY_SIZE_BYTES;
            let entry = u32::from_le_bytes(
                fat[offset..offset + FAT_ENTRY_SIZE_BYTES]
                    .try_into()
                    .unwrap(),
            );

            entry & FAT_ENTRY_MASK == 0
        })
        .count()
}

fn assert_fat_copies_equal(vfs: &mut Vfs) {
    let num_fats = vfs.driver.boot_sector.num_fats as u64;
    let first = read_fat_copy(vfs, 0);

    for fat_index in 1..num_fats {
        let other = read_fat_copy(vfs, fat_index);
        assert!(first == other, "FAT copy {} differs from FAT 0", fat_index);
    }
}

/// drops the mounted driver state and mounts again from the same block device
fn remount(vfs: &mut Vfs) {
    let device = replace(&mut vfs.driver.device, BlockDevice::Ram(RamDisk::new(0)));
    vfs.driver = Fat32Driver::new(device).expect("remount failed");
}

#[test_case]
fn test_bpb_sanity() {
    let vfs = get_vfs();
    let boot_sector = &vfs.driver.boot_sector;

    assert_eq!(boot_sector.bytes_per_sector, EXPECTED_BYTES_PER_SECTOR);
    assert_eq!(boot_sector.num_fats, EXPECTED_NUM_FATS);
    assert_eq!(boot_sector.root_cluster, EXPECTED_ROOT_CLUSTER);
}

#[test_case]
fn test_root_listing() {
    let mut vfs = get_vfs();
    let nodes = vfs.list_directory("/").expect("list_directory failed");

    let expected = [
        ("HELLO.TXT", FileType::File, HELLO_CONTENT.len()),
        ("EMPTY.TXT", FileType::File, 0),
        ("BIG.BIN", FileType::File, BIG_FILE_SIZE_BYTES),
        ("DIR1", FileType::Directory, 0),
    ];
    for (name, file_type, size) in expected {
        let node = nodes
            .iter()
            .find(|node| node.name == name)
            .unwrap_or_else(|| panic!("{} missing from the root listing", name));
        assert_eq!(node.file_type, file_type, "{}", name);
        assert_eq!(node.size, size, "{}", name);
    }
}

/// mcopy stores "a b.txt" as long name entries plus a short alias, only the alias is listed
#[test_case]
fn test_long_name_entries_are_skipped() {
    let mut vfs = get_vfs();
    let nodes = vfs
        .list_directory(LFN_DIRECTORY_PATH)
        .expect("listing a directory with long name entries failed");

    assert_eq!(nodes.len(), 1, "{:?}", nodes);
    let alias = &nodes[0];
    assert_eq!(alias.file_type, FileType::File);
    assert_eq!(alias.size, LFN_CONTENT.len());

    let alias_path = format!("{}/{}", LFN_DIRECTORY_PATH, alias.name);
    let content = read_exact(&mut vfs, &alias_path, 0, LFN_CONTENT.len());
    assert_eq!(content, LFN_CONTENT);
}

#[test_case]
fn test_hello_content() {
    let mut vfs = get_vfs();

    assert_eq!(file_size(&mut vfs, "/HELLO.TXT"), HELLO_CONTENT.len());
    let content = read_exact(&mut vfs, "/HELLO.TXT", 0, HELLO_CONTENT.len());
    assert_eq!(content, HELLO_CONTENT);
}

#[test_case]
fn test_empty_file() {
    let mut vfs = get_vfs();

    assert_eq!(file_size(&mut vfs, "/EMPTY.TXT"), 0);
    let mut buffer = [0u8; 16];
    let read = vfs
        .read_file("/EMPTY.TXT", 0, &mut buffer)
        .expect("read_file failed");
    assert_eq!(read, 0);

    let empty_handle = vfs.resolve_path("/EMPTY.TXT").unwrap().node_id;
    let other_handle = vfs.create_file("/EMPTY2.TXT").unwrap().node_id;
    assert_ne!(empty_handle, other_handle);

    vfs.delete("/EMPTY2.TXT").expect("delete failed");
}

#[test_case]
fn test_big_file_whole_read() {
    let mut vfs = get_vfs();

    assert_eq!(file_size(&mut vfs, "/BIG.BIN"), BIG_FILE_SIZE_BYTES);
    let content = read_exact(&mut vfs, "/BIG.BIN", 0, BIG_FILE_SIZE_BYTES);
    assert!(content == pattern_buffer(0, BIG_FILE_SIZE_BYTES));
}

#[test_case]
fn test_reads_across_cluster_boundaries() {
    let mut vfs = get_vfs();
    let cluster_size = cluster_size_bytes(&vfs);
    let lengths = [1, cluster_size - 1, cluster_size, cluster_size + 1];

    for boundary in (cluster_size..BIG_FILE_SIZE_BYTES).step_by(cluster_size) {
        for start in [boundary - 1, boundary] {
            for length in lengths {
                let length = length.min(BIG_FILE_SIZE_BYTES - start);
                let content = read_exact(&mut vfs, "/BIG.BIN", start, length);
                assert!(
                    content == pattern_buffer(start, length),
                    "mismatch at start {} length {}",
                    start,
                    length
                );
            }
        }
    }
}

#[test_case]
fn test_read_at_and_past_eof() {
    let mut vfs = get_vfs();
    let mut buffer = [0u8; EOF_CROSSING_REQUEST_BYTES];

    let read_at_eof = vfs
        .read_file("/BIG.BIN", BIG_FILE_SIZE_BYTES, &mut buffer)
        .unwrap();
    assert_eq!(read_at_eof, 0);

    let read_past_eof = vfs
        .read_file("/BIG.BIN", BIG_FILE_SIZE_BYTES + 1000, &mut buffer)
        .unwrap();
    assert_eq!(read_past_eof, 0);

    let start = BIG_FILE_SIZE_BYTES - EOF_CROSSING_TAIL_BYTES;
    let read_crossing = vfs.read_file("/BIG.BIN", start, &mut buffer).unwrap();
    assert_eq!(read_crossing, EOF_CROSSING_TAIL_BYTES);
    assert!(buffer[..read_crossing] == pattern_buffer(start, EOF_CROSSING_TAIL_BYTES));
}

#[test_case]
fn test_nested_paths() {
    let mut vfs = get_vfs();

    for path in [NESTED_FILE_PATH_1, NESTED_FILE_PATH_2] {
        let expected = format!("My path is {}", path);
        let content = read_exact(&mut vfs, path, 0, expected.len());
        assert_eq!(content, expected.as_bytes());
    }

    let mut buffer = [0u8; 8];
    assert_eq!(
        vfs.read_file("/DIR1/MISSING.TXT", 0, &mut buffer),
        Err(FileSystemError::NotFound)
    );
    assert_eq!(
        vfs.read_file("/MISSING/F1_1.TXT", 0, &mut buffer),
        Err(FileSystemError::NotFound)
    );
    assert_eq!(
        vfs.read_file("/HELLO.TXT/F1_1.TXT", 0, &mut buffer),
        Err(FileSystemError::NotDirectory)
    );
}

#[test_case]
fn test_create_write_read_small() {
    let mut vfs = get_vfs();
    let content = b"small write";

    vfs.create_file("/W_SMALL.TXT").expect("create failed");
    assert_eq!(file_size(&mut vfs, "/W_SMALL.TXT"), 0);

    write_all(&mut vfs, "/W_SMALL.TXT", 0, content);
    assert_eq!(file_size(&mut vfs, "/W_SMALL.TXT"), content.len());
    assert_eq!(
        read_exact(&mut vfs, "/W_SMALL.TXT", 0, content.len()),
        content
    );

    vfs.delete("/W_SMALL.TXT").expect("delete failed");
}

#[test_case]
fn test_write_growing_in_one_call() {
    let mut vfs = get_vfs();
    let size = GROW_CLUSTERS * cluster_size_bytes(&vfs) + GROW_EXTRA_BYTES;
    let data = pattern_buffer(0, size);

    vfs.create_file("/W_GROW.BIN").expect("create failed");
    write_all(&mut vfs, "/W_GROW.BIN", 0, &data);

    assert_eq!(file_size(&mut vfs, "/W_GROW.BIN"), size);
    assert!(read_exact(&mut vfs, "/W_GROW.BIN", 0, size) == data);

    vfs.delete("/W_GROW.BIN").expect("delete failed");
}

#[test_case]
fn test_append_small_writes() {
    let mut vfs = get_vfs();
    let size = APPEND_CLUSTERS * cluster_size_bytes(&vfs) + APPEND_CHUNK_BYTES / 2;
    let data = pattern_buffer(0, size);

    vfs.create_file("/W_APPEND.BIN").expect("create failed");
    let mut offset = 0;
    for chunk in data.chunks(APPEND_CHUNK_BYTES) {
        write_all(&mut vfs, "/W_APPEND.BIN", offset, chunk);
        offset += chunk.len();
        assert_eq!(file_size(&mut vfs, "/W_APPEND.BIN"), offset);
    }

    assert!(read_exact(&mut vfs, "/W_APPEND.BIN", 0, size) == data);

    vfs.delete("/W_APPEND.BIN").expect("delete failed");
}

#[test_case]
fn test_overwrite_in_the_middle() {
    let mut vfs = get_vfs();
    let cluster_size = cluster_size_bytes(&vfs);
    let start = OVERWRITE_START_CLUSTERS * cluster_size - OVERWRITE_START_BACKOFF_BYTES;
    let length = OVERWRITE_LENGTH_CLUSTERS * cluster_size + OVERWRITE_LENGTH_EXTRA_BYTES;
    let end = start + length;
    assert!(end < BIG_FILE_SIZE_BYTES);

    let original = read_exact(&mut vfs, "/BIG.BIN", 0, BIG_FILE_SIZE_BYTES);
    vfs.create_file("/W_OVER.BIN").expect("create failed");
    write_all(&mut vfs, "/W_OVER.BIN", 0, &original);

    write_all(
        &mut vfs,
        "/W_OVER.BIN",
        start,
        &vec![OVERWRITE_BYTE; length],
    );

    assert_eq!(file_size(&mut vfs, "/W_OVER.BIN"), BIG_FILE_SIZE_BYTES);
    let result = read_exact(&mut vfs, "/W_OVER.BIN", 0, BIG_FILE_SIZE_BYTES);
    assert!(result[..start] == original[..start], "bytes before changed");
    assert!(
        result[start..end]
            .iter()
            .all(|&byte| byte == OVERWRITE_BYTE),
        "overwritten range wrong"
    );
    assert!(result[end..] == original[end..], "bytes after changed");

    vfs.delete("/W_OVER.BIN").expect("delete failed");
}

#[test_case]
fn test_write_past_eof_is_rejected() {
    let mut vfs = get_vfs();
    let content = b"abc";

    vfs.create_file("/W_PAST.TXT").expect("create failed");
    write_all(&mut vfs, "/W_PAST.TXT", 0, content);

    assert_eq!(
        vfs.write_file("/W_PAST.TXT", content.len() + PAST_EOF_GAP_BYTES, b"x"),
        Err(FileSystemError::OffsetPastEnd)
    );
    assert_eq!(file_size(&mut vfs, "/W_PAST.TXT"), content.len());

    vfs.delete("/W_PAST.TXT").expect("delete failed");
}

#[test_case]
fn test_directory_lifecycle() {
    let mut vfs = get_vfs();

    vfs.create_directory("/NS_DIR").expect("mkdir failed");
    vfs.create_file("/NS_DIR/A.TXT").expect("create failed");
    write_all(&mut vfs, "/NS_DIR/A.TXT", 0, b"inside");

    assert_eq!(list_names(&mut vfs, "/NS_DIR"), ["A.TXT"]);
    assert!(
        list_names(&mut vfs, "/")
            .iter()
            .any(|name| name == "NS_DIR")
    );

    vfs.delete("/NS_DIR/A.TXT").expect("delete file failed");
    assert!(list_names(&mut vfs, "/NS_DIR").is_empty());
    vfs.delete("/NS_DIR").expect("delete directory failed");

    assert!(
        !list_names(&mut vfs, "/")
            .iter()
            .any(|name| name == "NS_DIR")
    );
    assert_eq!(
        vfs.resolve_path("/NS_DIR").map(|node| node.name),
        Err(FileSystemError::NotFound)
    );
}

#[test_case]
fn test_delete_non_empty_directory() {
    let mut vfs = get_vfs();

    vfs.create_directory("/NE_DIR").expect("mkdir failed");
    vfs.create_file("/NE_DIR/A.TXT").expect("create failed");

    assert_eq!(
        vfs.delete("/NE_DIR"),
        Err(FileSystemError::DirectoryNotEmpty)
    );

    vfs.delete("/NE_DIR/A.TXT").expect("delete file failed");
    vfs.delete("/NE_DIR").expect("delete directory failed");
}

#[test_case]
fn test_create_existing_name() {
    let mut vfs = get_vfs();

    assert_eq!(
        vfs.create_file("/HELLO.TXT").map(|node| node.name),
        Err(FileSystemError::FileExists)
    );
    assert_eq!(
        vfs.create_file("/hello.txt").map(|node| node.name),
        Err(FileSystemError::FileExists)
    );
    assert_eq!(
        vfs.create_directory("/DIR1").map(|node| node.name),
        Err(FileSystemError::FileExists)
    );
    assert_eq!(file_size(&mut vfs, "/HELLO.TXT"), HELLO_CONTENT.len());
}

#[test_case]
fn test_invalid_names() {
    let mut vfs = get_vfs();
    let invalid_names = [
        "/TOOLONGNAME.TXT",
        "/STEM.LONG",
        "/.HIDDEN",
        "/A.B.C",
        "/BAD*.TXT",
    ];

    for path in invalid_names {
        assert_eq!(
            vfs.create_file(path).map(|node| node.name),
            Err(FileSystemError::InvalidFilename),
            "{}",
            path
        );
    }
}

#[test_case]
fn test_directory_growing_past_one_cluster() {
    let mut vfs = get_vfs();
    let entries_per_cluster = cluster_size_bytes(&vfs) / DIRECTORY_ENTRY_SIZE_BYTES;
    let file_count = 2 * entries_per_cluster + EXTRA_DIRECTORY_ENTRIES;

    vfs.create_directory("/GROW_DIR").expect("mkdir failed");
    for index in 0..file_count {
        let path = format!("/GROW_DIR/F{:04}.TXT", index);
        vfs.create_file(&path).expect("create failed");
        write_all(&mut vfs, &path, 0, path.as_bytes());
    }

    let names = list_names(&mut vfs, "/GROW_DIR");
    assert_eq!(names.len(), file_count);
    for index in 0..file_count {
        let name = format!("F{:04}.TXT", index);
        assert!(names.contains(&name), "{} missing", name);
    }

    let last_path = format!("/GROW_DIR/F{:04}.TXT", file_count - 1);
    assert_eq!(
        read_exact(&mut vfs, &last_path, 0, last_path.len()),
        last_path.as_bytes()
    );

    for index in 0..file_count {
        vfs.delete(&format!("/GROW_DIR/F{:04}.TXT", index))
            .expect("delete failed");
    }
    assert!(list_names(&mut vfs, "/GROW_DIR").is_empty());
    vfs.delete("/GROW_DIR").expect("delete directory failed");
}

#[test_case]
fn test_fat_copies_match_after_writes() {
    let mut vfs = get_vfs();
    let size = GROW_CLUSTERS * cluster_size_bytes(&vfs);

    vfs.create_directory("/FC_DIR").expect("mkdir failed");
    vfs.create_file("/FC_DIR/A.BIN").expect("create failed");
    write_all(&mut vfs, "/FC_DIR/A.BIN", 0, &pattern_buffer(0, size));
    vfs.create_file("/FC_DIR/B.BIN").expect("create failed");
    write_all(&mut vfs, "/FC_DIR/B.BIN", 0, &pattern_buffer(1, size));
    vfs.delete("/FC_DIR/A.BIN").expect("delete failed");

    assert_fat_copies_equal(&mut vfs);

    vfs.delete("/FC_DIR/B.BIN").expect("delete failed");
    vfs.delete("/FC_DIR").expect("delete failed");
    assert_fat_copies_equal(&mut vfs);
}

#[test_case]
fn test_free_cluster_count_is_restored() {
    let mut vfs = get_vfs();
    let size = GROW_CLUSTERS * cluster_size_bytes(&vfs) + GROW_EXTRA_BYTES;
    let free_before = count_free_clusters(&mut vfs);

    vfs.create_directory("/FREE_DIR").expect("mkdir failed");
    vfs.create_file("/FREE_DIR/A.BIN").expect("create failed");
    write_all(&mut vfs, "/FREE_DIR/A.BIN", 0, &pattern_buffer(0, size));
    assert!(count_free_clusters(&mut vfs) < free_before);

    vfs.delete("/FREE_DIR/A.BIN").expect("delete failed");
    vfs.delete("/FREE_DIR").expect("delete failed");
    assert_eq!(count_free_clusters(&mut vfs), free_before);
}

#[test_case]
fn test_remount_keeps_data() {
    let mut vfs = get_vfs();
    let size = REMOUNT_CLUSTERS * cluster_size_bytes(&vfs) + GROW_EXTRA_BYTES;
    let data = pattern_buffer(0, size);

    vfs.create_directory("/RM_DIR").expect("mkdir failed");
    vfs.create_file("/RM_DIR/DATA.BIN").expect("create failed");
    write_all(&mut vfs, "/RM_DIR/DATA.BIN", 0, &data);

    remount(&mut vfs);

    assert_eq!(file_size(&mut vfs, "/RM_DIR/DATA.BIN"), size);
    assert!(read_exact(&mut vfs, "/RM_DIR/DATA.BIN", 0, size) == data);
    assert!(
        read_exact(&mut vfs, "/BIG.BIN", 0, BIG_FILE_SIZE_BYTES)
            == pattern_buffer(0, BIG_FILE_SIZE_BYTES)
    );

    vfs.delete("/RM_DIR/DATA.BIN").expect("delete failed");
    vfs.delete("/RM_DIR").expect("delete failed");
}
