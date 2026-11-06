use crate::filesystem::fat32::direntry::FatFileAttributes;
use crate::serial_println;
extern crate alloc;
use crate::filesystem::fat32::{END_OF_CHAIN, ROOT_CLUSTER};
use alloc::alloc::{Layout, alloc};
use alloc::boxed::Box;
use core::ptr;

const IMAGE_SIZE: usize = 64 * 1024;

#[inline(never)]
pub fn create_fat32_image() -> Box<[u8; IMAGE_SIZE]> {
    serial_println!("Allocating FAT32 image - size: {}", IMAGE_SIZE);

    let layout = Layout::new::<[u8; IMAGE_SIZE]>();
    let ptr = unsafe { alloc(layout) as *mut [u8; IMAGE_SIZE] };
    if ptr.is_null() {
        panic!("Allocation failed");
    }

    unsafe {
        ptr::write_bytes(ptr as *mut u8, 0, IMAGE_SIZE);
    }

    let mut image = unsafe { Box::from_raw(ptr) };
    serial_println!("FAT32 image allocated - size: {}", IMAGE_SIZE);

    const BYTES_PER_SECTOR: usize = 512;
    const SECTORS_PER_CLUSTER: usize = 8;
    const RESERVED_SECTORS: usize = 32;
    const FAT_COUNT: usize = 2;
    const SECTORS_PER_FAT: usize = 8; // 8 sectors = 4KB FAT table
    const FAT_SIZE_BYTES: usize = SECTORS_PER_FAT * BYTES_PER_SECTOR;

    let fat1_start = RESERVED_SECTORS;
    let fat2_start = RESERVED_SECTORS + SECTORS_PER_FAT;
    let data_start = RESERVED_SECTORS + (SECTORS_PER_FAT * FAT_COUNT);

    // Root directory cluster is 2 (first cluster of data area)
    let root_cluster: usize = ROOT_CLUSTER as usize;
    let root_cluster_sector = data_start + ((root_cluster - 2) * SECTORS_PER_CLUSTER);

    image[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    image[3..11].copy_from_slice(b"MOFUOS01");
    image[11..13].copy_from_slice(&(BYTES_PER_SECTOR as u16).to_le_bytes());
    image[13] = SECTORS_PER_CLUSTER as u8;
    image[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    image[16] = FAT_COUNT as u8;
    image[17..19].copy_from_slice(&0u16.to_le_bytes());
    image[19..21].copy_from_slice(&0u16.to_le_bytes());
    image[21] = 0xF8;
    image[22..24].copy_from_slice(&0u16.to_le_bytes());
    // Sectors per track
    image[24..26].copy_from_slice(&63u16.to_le_bytes());
    // Number of heads
    image[26..28].copy_from_slice(&255u16.to_le_bytes());
    // Hidden sectors
    image[28..32].copy_from_slice(&2048u32.to_le_bytes());
    serial_println!("Wrote to index 12");

    let total_sector_count = (IMAGE_SIZE / BYTES_PER_SECTOR) as u32;
    image[32..36].copy_from_slice(&total_sector_count.to_le_bytes());
    image[36..40].copy_from_slice(&(SECTORS_PER_FAT as u32).to_le_bytes());

    // Extended flags
    image[40..42].copy_from_slice(&0x8000u16.to_le_bytes());

    image[42..44].copy_from_slice(&0u16.to_le_bytes());
    image[44..48].copy_from_slice(&(root_cluster as u32).to_le_bytes());

    // FSInfo sector (sector 1)
    image[48..50].copy_from_slice(&1u16.to_le_bytes());

    // Backup boot sector (none)
    image[50..52].copy_from_slice(&0u16.to_le_bytes());

    // Reserved
    image[52..64].fill(0);
    // Drive number
    image[64] = 0x80;
    // Reserved
    image[65] = 0;
    // Boot signature
    image[66] = 0x29;
    // Volume serial number
    image[67..71].copy_from_slice(&0x10203040u32.to_le_bytes());

    // Volume label
    image[71..82].copy_from_slice(b"VOLUME     ");

    // Filesystem type
    image[82..90].copy_from_slice(b"FAT32   ");

    // Boot signature
    image[510..512].copy_from_slice(&0xAA55u16.to_le_bytes());

    // FSInfo sector
    let mut fsinfo = [0u8; 512];
    fsinfo[0..4].copy_from_slice(&0x41615252u32.to_le_bytes());
    fsinfo[484..488].copy_from_slice(&0x61417272u32.to_le_bytes());
    // Free cluster count unknown
    fsinfo[488..492].copy_from_slice(&0xFFFFFFFFu32.to_le_bytes());
    // Next free cluster
    fsinfo[492..496].copy_from_slice(&3u32.to_le_bytes());
    fsinfo[508..512].copy_from_slice(&0xAA55u32.to_le_bytes());

    image[512..1024].copy_from_slice(&fsinfo);

    // Create FAT table
    let mut fat = [0u8; FAT_SIZE_BYTES];

    // Entry 0: Media descriptor
    fat[0..4].copy_from_slice(&END_OF_CHAIN.to_le_bytes());

    // Entry 1: Reserved
    fat[4..8].copy_from_slice(&END_OF_CHAIN.to_le_bytes());

    // Entry 2: Root directory cluster (end of chain)
    fat[8..12].copy_from_slice(&END_OF_CHAIN.to_le_bytes());

    // Copy FAT tables
    let fat1_offset = fat1_start * BYTES_PER_SECTOR;
    let fat2_offset = fat2_start * BYTES_PER_SECTOR;
    image[fat1_offset..fat1_offset + FAT_SIZE_BYTES].copy_from_slice(&fat);
    image[fat2_offset..fat2_offset + FAT_SIZE_BYTES].copy_from_slice(&fat);

    // Create root directory at cluster 2
    let root_offset = root_cluster_sector * BYTES_PER_SECTOR;

    // Directory entry for "hello.txt"
    let mut root_dir = [0u8; FAT_SIZE_BYTES];

    // Volume label entry
    let vol_name = b"VOLUME     ";
    root_dir[0..11].copy_from_slice(vol_name);
    root_dir[11] = FatFileAttributes::VolumeId as u8;

    let name = b"HELLO   ";
    let ext = b"txt";
    let hello_msg = b"Hello from MofuOS!";
    let entry_offset = 32;

    let nam2 = b"SOMEFILE";
    let ext2 = b"bin";
    let bin_data = b"binarydatathatsprettyshortforbinarydatabutitsamockone";
    let entry2_offset = 64;

    root_dir[entry_offset..entry_offset + 8].copy_from_slice(name);
    root_dir[entry_offset + 8..entry_offset + 11].copy_from_slice(ext);
    root_dir[entry_offset + 11] = 0x20; // Archive attribute
    root_dir[entry_offset + 26..entry_offset + 28].copy_from_slice(&3u16.to_le_bytes()); // First cluster low (cluster 3)
    root_dir[entry_offset + 28..entry_offset + 32]
        .copy_from_slice(&(hello_msg.len() as u32).to_le_bytes());

    root_dir[entry2_offset..entry2_offset + 8].copy_from_slice(nam2);
    root_dir[entry2_offset + 8..entry2_offset + 11].copy_from_slice(ext2);
    root_dir[entry2_offset + 11] = 0x20; // Archive attribute
    root_dir[entry2_offset + 26..entry2_offset + 28].copy_from_slice(&4u16.to_le_bytes()); // First cluster low (cluster 4)
    root_dir[entry2_offset + 28..entry2_offset + 32]
        .copy_from_slice(&(bin_data.len() as u32).to_le_bytes());

    // Copy root directory
    image[root_offset..root_offset + root_dir.len()].copy_from_slice(&root_dir);

    // Create file data at cluster 3
    let file_cluster: usize = 3;
    let file_cluster_sector = data_start + ((file_cluster - 2) * SECTORS_PER_CLUSTER);
    let file_offset = file_cluster_sector * BYTES_PER_SECTOR;

    let file2_cluster: usize = 4;
    let file2_cluster_sector = data_start + ((file2_cluster - 2) * SECTORS_PER_CLUSTER);
    let file2_offset = file2_cluster_sector * BYTES_PER_SECTOR;

    image[file2_offset..file2_offset + bin_data.len()].copy_from_slice(bin_data);
    image[file_offset..file_offset + hello_msg.len()].copy_from_slice(hello_msg);

    // Update FAT to mark cluster 3 as EOC
    let fat_entry_offset = file_cluster * 4; // Each FAT entry is 4 bytes
    let fat1_entry_offset = fat1_offset + fat_entry_offset;
    let fat2_entry_offset = fat2_offset + fat_entry_offset;

    image[fat1_entry_offset..fat1_entry_offset + 4].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
    image[fat2_entry_offset..fat2_entry_offset + 4].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
    let cluster4_entry_offset = 4 * 4;
    image[fat1_offset + cluster4_entry_offset..fat1_offset + cluster4_entry_offset + 4]
        .copy_from_slice(&END_OF_CHAIN.to_le_bytes());
    image[fat2_offset + cluster4_entry_offset..fat2_offset + cluster4_entry_offset + 4]
        .copy_from_slice(&END_OF_CHAIN.to_le_bytes());

    serial_println!("FAT32 image creation complete");
    serial_println!("    Total sectors: {}", total_sector_count);
    serial_println!("    Reserved sectors: {}", RESERVED_SECTORS);
    serial_println!("    Sectors per FAT: {}", SECTORS_PER_FAT);
    serial_println!("    Data start sector: {}", data_start);
    serial_println!("    Root directory cluster: {}", root_cluster);
    serial_println!("    Root directory sector: {}", root_cluster_sector);
    serial_println!("    File at cluster: {}", file_cluster);

    image
}
