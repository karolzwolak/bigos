use crate::filesystem::fat32::direntry::{DirectoryEntry, FatFileAttributes};
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
    image[3..11].copy_from_slice(b"BigOS123");
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

    // Entry 3: hello.txt (end of chain)
    fat[12..16].copy_from_slice(&END_OF_CHAIN.to_le_bytes());

    // Entry 4: lore.txt (end of chain)
    fat[16..20].copy_from_slice(&END_OF_CHAIN.to_le_bytes());

    // Entry 5: nested_dir directory (end of chain)
    fat[20..24].copy_from_slice(&END_OF_CHAIN.to_le_bytes());

    // Entry 6: file inside nested_dir (end of chain)
    fat[24..28].copy_from_slice(&END_OF_CHAIN.to_le_bytes());

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
    let ext = b"TXT";
    let hello_msg = b"Hello from BigOS!";
    let entry_offset = 32;

    let nam2 = b"LORE    ";
    let ext2 = b"TXT";
    let bin_data = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. Vestibulum eget lobortis risus. Duis molestie enim at ullamcorper gravida. Praesent vel mollis arcu, ornare tincidunt enim. Vivamus luctus convallis urna ac consectetur. Orci varius natoque penatibus et magnis dis parturient montes, nascetur ridiculus mus. Donec pharetra lectus velit, ac tristique lacus placerat id. Vestibulum lobortis imperdiet ultrices.\n
Donec finibus justo quis cursus iaculis. Donec ut risus non tortor malesuada fringilla. Nullam lectus nunc, vestibulum non efficitur sit amet, mollis et purus. Aenean ultrices enim id nisl ultricies, at euismod tortor lobortis. Cras ut suscipit diam. In hac habitasse platea dictumst. Phasellus non tincidunt mi.\n
Duis ac dui eros. Aenean quis felis metus. Donec euismod ipsum quis sagittis vulputate. Proin et lectus tincidunt, malesuada leo a, tempus nisl. In dolor ipsum, gravida vitae pellentesque quis, egestas id ligula. Cras tristique et est gravida blandit. Pellentesque vitae justo vel sapien tristique vestibulum nec in lorem.\n
Curabitur id erat eu massa imperdiet rutrum. Praesent blandit leo nec dapibus semper. Donec id vulputate mi. Pellentesque rutrum aliquam justo, quis rutrum dui molestie hendrerit. Mauris bibendum leo id felis porttitor tincidunt. Sed porttitor aliquam sem ut facilisis. Cras ultricies ipsum eu ultrices placerat. Fusce non malesuada lectus.\n
Sed id dui fringilla, tincidunt neque scelerisque, pharetra dolor. Sed ultrices venenatis tellus. Nulla dui quam, fermentum quis dignissim sed, commodo a dui. Quisque eu fringilla velit, non finibus tortor morbi.";
    let entry2_offset = 64;

    root_dir[entry_offset..entry_offset + 8].copy_from_slice(name);
    root_dir[entry_offset + 8..entry_offset + 11].copy_from_slice(ext);
    root_dir[entry_offset + 11] = FatFileAttributes::Archive as u8;
    root_dir[entry_offset + 26..entry_offset + 28].copy_from_slice(&3u16.to_le_bytes()); // First cluster low (cluster 3)
    root_dir[entry_offset + 28..entry_offset + 32]
        .copy_from_slice(&(hello_msg.len() as u32).to_le_bytes());

    root_dir[entry2_offset..entry2_offset + 8].copy_from_slice(nam2);
    root_dir[entry2_offset + 8..entry2_offset + 11].copy_from_slice(ext2);
    root_dir[entry2_offset + 11] = FatFileAttributes::Archive as u8;
    root_dir[entry2_offset + 26..entry2_offset + 28].copy_from_slice(&4u16.to_le_bytes()); // First cluster low (cluster 4)
    root_dir[entry2_offset + 28..entry2_offset + 32]
        .copy_from_slice(&(bin_data.len() as u32).to_le_bytes());

    // nested_dir directory entry
    let nested_dir_name: &[u8; 8] = b"NESTDIR ";
    let nested_dir_ext = b"   ";
    let nested_dir_offset = 96;

    root_dir[nested_dir_offset..nested_dir_offset + 8].copy_from_slice(nested_dir_name);
    root_dir[nested_dir_offset + 8..nested_dir_offset + 11].copy_from_slice(nested_dir_ext);
    root_dir[nested_dir_offset + 11] = FatFileAttributes::Directory as u8;
    root_dir[nested_dir_offset + 26..nested_dir_offset + 28].copy_from_slice(&5u16.to_le_bytes()); // First cluster low (cluster 5)

    // Copy root directory
    image[root_offset..root_offset + root_dir.len()].copy_from_slice(&root_dir);

    // Create file data at cluster 3
    let file_cluster: usize = 3;
    let file_cluster_sector = data_start + ((file_cluster - 2) * SECTORS_PER_CLUSTER);
    let file_offset = file_cluster_sector * BYTES_PER_SECTOR;

    let file2_cluster: usize = 4;
    let file2_cluster_sector = data_start + ((file2_cluster - 2) * SECTORS_PER_CLUSTER);
    let file2_offset = file2_cluster_sector * BYTES_PER_SECTOR;

    let nested_dir_cluster: usize = 5;
    let nested_dir_cluster_sector = data_start + ((nested_dir_cluster - 2) * SECTORS_PER_CLUSTER);
    let nested_dir_offset = nested_dir_cluster_sector * BYTES_PER_SECTOR;

    let mut nested_dir_dir = [0u8; FAT_SIZE_BYTES];
    // "." entry
    let dot_name = b".       ";
    nested_dir_dir[0..8].copy_from_slice(dot_name);
    nested_dir_dir[8..11].copy_from_slice(b"   ");
    nested_dir_dir[11] = FatFileAttributes::Directory as u8;
    nested_dir_dir[26..28].copy_from_slice(&5u16.to_le_bytes()); // Points to itself (cluster 5)
    // ".." entry
    let dotdot_name = b"..      ";
    nested_dir_dir[32..40].copy_from_slice(dotdot_name);
    nested_dir_dir[40..43].copy_from_slice(b"   ");
    nested_dir_dir[43] = FatFileAttributes::Directory as u8;
    nested_dir_dir[58..60].copy_from_slice(&2u16.to_le_bytes()); // Points to root (cluster 2)
    // nestfile.txt entry inside nested_dir
    let nestfile_name = b"NESTFILE";
    let nestfile_ext = b"TXT";
    let nestfile_msg = b"Mauris erat urna, tempus vel porta ut, dapibus et tortor. Pellentesque id sem vitae erat pharetra blandit et ut lorem. Cras condimentum, nulla nec tempor mattis, dui ipsum posuere tellus, ac maximus orci arcu nec leo. Curabitur a consectetur augue blandit.";
    let nestfile_entry_offset = 64;

    nested_dir_dir[nestfile_entry_offset..nestfile_entry_offset + 8].copy_from_slice(nestfile_name);
    nested_dir_dir[nestfile_entry_offset + 8..nestfile_entry_offset + 11]
        .copy_from_slice(nestfile_ext);
    nested_dir_dir[nestfile_entry_offset + 11] = FatFileAttributes::Archive as u8;
    nested_dir_dir[nestfile_entry_offset + 26..nestfile_entry_offset + 28]
        .copy_from_slice(&6u16.to_le_bytes()); // First cluster low (cluster 6)
    nested_dir_dir[nestfile_entry_offset + 28..nestfile_entry_offset + 32]
        .copy_from_slice(&(nestfile_msg.len() as u32).to_le_bytes());

    // Create nestfile.txt data at cluster 6
    let nestfile_cluster: usize = 6;
    let nestfile_cluster_sector = data_start + ((nestfile_cluster - 2) * SECTORS_PER_CLUSTER);
    let nestfile_data_offset = nestfile_cluster_sector * BYTES_PER_SECTOR;

    image[file2_offset..file2_offset + bin_data.len()].copy_from_slice(bin_data);
    image[file_offset..file_offset + hello_msg.len()].copy_from_slice(hello_msg);
    image[nested_dir_offset..nested_dir_offset + nested_dir_dir.len()]
        .copy_from_slice(&nested_dir_dir);
    image[nestfile_data_offset..nestfile_data_offset + nestfile_msg.len()]
        .copy_from_slice(nestfile_msg);

    // Update FAT to mark cluster 3 as EOC
    let fat_entry_offset = file_cluster * 4; // Each FAT entry is 4 bytes
    let fat1_entry_offset = fat1_offset + fat_entry_offset;
    let fat2_entry_offset = fat2_offset + fat_entry_offset;

    image[fat1_offset + 12..fat1_offset + 16].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
    image[fat2_offset + 12..fat2_offset + 16].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
    image[fat1_offset + 16..fat1_offset + 20].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
    image[fat2_offset + 16..fat2_offset + 20].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
    image[fat1_offset + 20..fat1_offset + 24].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
    image[fat2_offset + 20..fat2_offset + 24].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
    image[fat1_offset + 24..fat1_offset + 28].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
    image[fat2_offset + 24..fat2_offset + 28].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
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
