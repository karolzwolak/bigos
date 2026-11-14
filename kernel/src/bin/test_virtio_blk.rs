#![no_std]
#![no_main]

extern crate alloc;
extern crate kernel;

use alloc::vec;
use core::panic::PanicInfo;
use kernel::{
    io::{
        disk::SECTOR_SIZE_BYTES,
        virtio::{MAX_REQUEST_SECTORS, VirtioBlk},
    },
    testing::{test_case, test_panic_handler},
};
use spin::{Mutex, MutexGuard, Once};

const TEST_DISK_SIZE_MB: u64 = 64;
const TEST_DISK_SECTORS: u64 = TEST_DISK_SIZE_MB * 1024 * 1024 / SECTOR_SIZE_BYTES as u64;

const BOOT_SIGNATURE_OFFSET: usize = 510;
const BOOT_SIGNATURE: [u8; 2] = [0x55, 0xAA];
const FAT32_TYPE_OFFSET: usize = 82;
const FAT32_TYPE_STRING: &[u8] = b"FAT32   ";

const MULTI_SECTOR_COUNT: usize = 64;
const SPLIT_WRITE_SECTORS: usize = MAX_REQUEST_SECTORS + MAX_REQUEST_SECTORS / 2;
const SPLIT_WRITE_SECTORS_FROM_END: u64 = 1024;
const ODD_BUFFER_OFFSET: usize = 3;
const ODD_BUFFER_SECTORS: usize = 4;
const BACK_TO_BACK_READS: u64 = 1000;

const PATTERN_MODULUS: usize = 251;
const PATTERN_XOR: u8 = 0x5A;

static DISK: Once<Mutex<VirtioBlk>> = Once::new();

fn disk() -> MutexGuard<'static, VirtioBlk> {
    DISK.get().unwrap().lock()
}

fn pattern_byte(index: usize, seed: u8) -> u8 {
    (index % PATTERN_MODULUS) as u8 ^ PATTERN_XOR ^ seed
}

fn fill_pattern(buffer: &mut [u8], seed: u8) {
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = pattern_byte(index, seed);
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    unsafe { kernel::boot_common::bsp_early_init() };

    let device = VirtioBlk::find_and_init().expect("virtio-blk device not found");
    DISK.call_once(|| Mutex::new(device));

    kernel::testing::run_all_tests()
}

#[test_case]
fn test_device_capacity() {
    assert_eq!(disk().sector_count(), TEST_DISK_SECTORS);
}

#[test_case]
fn test_boot_sector() {
    let mut sector = [0u8; SECTOR_SIZE_BYTES];
    disk()
        .read_sectors(0, &mut sector)
        .expect("read of sector 0 failed");

    assert_eq!(
        sector[BOOT_SIGNATURE_OFFSET..BOOT_SIGNATURE_OFFSET + BOOT_SIGNATURE.len()],
        BOOT_SIGNATURE
    );
    assert_eq!(
        &sector[FAT32_TYPE_OFFSET..FAT32_TYPE_OFFSET + FAT32_TYPE_STRING.len()],
        FAT32_TYPE_STRING
    );
}

#[test_case]
fn test_write_read_last_sector() {
    let last_sector = TEST_DISK_SECTORS - 1;
    let mut written = [0u8; SECTOR_SIZE_BYTES];
    fill_pattern(&mut written, 1);

    let mut disk = disk();
    disk.write_sectors(last_sector, &written)
        .expect("write of the last sector failed");

    let mut read_back = [0u8; SECTOR_SIZE_BYTES];
    disk.read_sectors(last_sector, &mut read_back)
        .expect("read of the last sector failed");
    assert_eq!(written, read_back);
}

#[test_case]
fn test_out_of_range_is_rejected() {
    let mut sector = [0u8; SECTOR_SIZE_BYTES];

    assert!(disk().read_sectors(TEST_DISK_SECTORS, &mut sector).is_err());
    assert!(disk().write_sectors(TEST_DISK_SECTORS, &sector).is_err());
}

#[test_case]
fn test_multi_sector_read_matches_single_reads() {
    let mut disk = disk();

    let mut multi = vec![0u8; MULTI_SECTOR_COUNT * SECTOR_SIZE_BYTES];
    disk.read_sectors(0, &mut multi)
        .expect("multi sector read failed");

    let mut single = [0u8; SECTOR_SIZE_BYTES];
    for sector_index in 0..MULTI_SECTOR_COUNT {
        disk.read_sectors(sector_index as u64, &mut single)
            .expect("single sector read failed");

        let start = sector_index * SECTOR_SIZE_BYTES;
        assert_eq!(
            multi[start..start + SECTOR_SIZE_BYTES],
            single,
            "sector {} differs",
            sector_index
        );
    }
}

#[test_case]
fn test_write_across_request_split() {
    let start_sector = TEST_DISK_SECTORS - SPLIT_WRITE_SECTORS_FROM_END;
    let mut written = vec![0u8; SPLIT_WRITE_SECTORS * SECTOR_SIZE_BYTES];
    fill_pattern(&mut written, 2);

    let mut disk = disk();
    let requests_before = disk.requests_completed;
    disk.write_sectors(start_sector, &written)
        .expect("split write failed");
    assert!(
        disk.requests_completed - requests_before >= 2,
        "write was not split into several requests"
    );

    let mut read_back = vec![0u8; written.len()];
    disk.read_sectors(start_sector, &mut read_back)
        .expect("split read failed");
    assert!(written == read_back, "data differs after the split write");
}

#[test_case]
fn test_read_into_odd_heap_offset() {
    let mut disk = disk();
    let mut expected = vec![0u8; ODD_BUFFER_SECTORS * SECTOR_SIZE_BYTES];
    disk.read_sectors(0, &mut expected)
        .expect("reference read failed");

    let mut backing = vec![0u8; ODD_BUFFER_OFFSET + expected.len()];
    let odd_buffer = &mut backing[ODD_BUFFER_OFFSET..];
    assert!(!(odd_buffer.as_ptr() as usize).is_multiple_of(SECTOR_SIZE_BYTES));
    disk.read_sectors(0, odd_buffer)
        .expect("read into an odd offset failed");

    assert!(odd_buffer == expected.as_slice());
}

#[test_case]
fn test_back_to_back_reads() {
    let mut disk = disk();
    let requests_before = disk.requests_completed;

    let mut sector = [0u8; SECTOR_SIZE_BYTES];
    for read_index in 0..BACK_TO_BACK_READS {
        disk.read_sectors(read_index % MULTI_SECTOR_COUNT as u64, &mut sector)
            .expect("back to back read failed");
    }

    assert_eq!(
        disk.requests_completed - requests_before,
        BACK_TO_BACK_READS
    );
}
