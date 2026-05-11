use crate::filesystem::sirius::{FileSystemError, FileSystemResult};
use crate::serial_println;

// FAT32 Boot Sector (the first 512 bytes of a volume)
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct BootSector {
    pub jmp_boot: [u8; 3],       // offset 0x00: Jump instruction
    pub oem_name: [u8; 8],       // offset 0x03: OEM identifier
    pub bytes_per_sector: u16, // offset 0x0B: 512, 1024, 2048 or 4096; must be the same as the sector size of the storage
    pub sectors_per_cluster: u8, // offset 0x0D: number of sectors per allocation unit; a power of 2
    pub reserved_sectors: u16, // offset 0x0E: Reserved sectors before FAT (32 for FAT32)
    pub num_fats: u8,          // offset 0x10: Number of FAT copies (2)
    pub root_entries: u16,     // offset 0x11: Max root dir entries (0 for FAT32)
    pub total_sectors_16: u16, // offset 0x13: Total sectors (0 for FAT32, use total_sectors_32)
    pub media_descriptor: u8, // offset 0x15: Media descriptor byte; the same value has to be in the low 8 bits of FAT[0];
    // valid values: 0xF0, 0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE and 0xFF. 0xF8 is the common value for partitioned disk
    pub fat_size_16: u16, // offset 0x16: FAT size in sectors (0 for FAT32, fat_size_32 is used instead)
    pub sectors_per_track: u16, // offset 0x18: Sectors per track
    pub num_heads: u16,   // offset 0x1A: Number of heads
    pub hidden_sectors: u32, // offset 0x1C: Hidden sectors count (0 if volume is located at the beginning of the disk)
    pub total_sectors_32: u32, // offset 0x20: Volume size - total number of sectors of the FAT volume
    pub fat_size_32: u32, // offset 0x24: FAT size in sectors (size of the FAT area is fat_size_32 * num_fats)
    pub ext_flags: u16,   // offset 0x28: Extended flags (bits 0-3: active FAT, bits 4-6: reserved,
    // bit 7: 0 - each FAT is active and mirrored, 1 - only one FAT indicated by bits 0-3 is active)
    pub fs_version: u16,         // offset 0x2A: Filesystem version (0)
    pub root_cluster: u32, // offset 0x2C: First cluster number of the root directory (usually 2)
    pub fsinfo_sector: u16, // offset 0x30: FSInfo sector number (usually 1)
    pub backup_boot_sector: u16, // offset 0x32: Backup boot sector number (usually 6)
    pub reserved: [u8; 12], // offset 0x34: Reserved
    pub drive_number: u8,  // offset 0x40: Drive number (0x00 for floppy disk, 0x80 for fixed disk)
    pub winnt_flags: u8,   // offset 0x41: Windows NT flags
    pub signature: u8,     // offset 0x42: Signature (0x29)
    pub volume_id: u32,    // offset 0x43: Volume serial number
    pub volume_label: [u8; 11], // offset 0x47: Volume label
    pub fs_type: [u8; 8],  // offset 0x52: Filesystem type (always "FAT32   ")
    pub bootcode: [u8; 420], // offset 0x5A: Bootstrap program
    pub signature_word: u16, // offset 0x1FE: Boot sector signature (0xAA55)

    pub root_dir_sectors: u16, // Calculated from root_entries
}

impl BootSector {
    pub fn from_bytes(data: &[u8]) -> FileSystemResult<Self> {
        if data.len() < 512 {
            serial_println!("BootSector::from_bytes: data.len() < 512");
            return Err(FileSystemError::IoError);
        }

        // check the Boot sector signature
        // let signature = u16::from_le_bytes([data[510], data[511]]);
        // if signature != 0xAA55 {
        //     serial_println!("BootSector::from_bytes: signature != 0xAA55");
        //     return Err(FileSystemError::DiskOpError);
        // }

        let bs = BootSector {
            jmp_boot: [data[0], data[1], data[2]],
            oem_name: [
                data[3], data[4], data[5], data[6], data[7], data[8], data[9], data[10],
            ],
            bytes_per_sector: u16::from_le_bytes([data[11], data[12]]),
            sectors_per_cluster: data[13],
            reserved_sectors: u16::from_le_bytes([data[14], data[15]]),
            num_fats: data[16],
            root_entries: u16::from_le_bytes([data[17], data[18]]),
            total_sectors_16: u16::from_le_bytes([data[19], data[20]]),
            media_descriptor: data[21],
            fat_size_16: u16::from_le_bytes([data[22], data[23]]),
            sectors_per_track: u16::from_le_bytes([data[24], data[25]]),
            num_heads: u16::from_le_bytes([data[26], data[27]]),
            hidden_sectors: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
            total_sectors_32: u32::from_le_bytes([data[32], data[33], data[34], data[35]]),
            fat_size_32: u32::from_le_bytes([data[36], data[37], data[38], data[39]]),
            ext_flags: u16::from_le_bytes([data[40], data[41]]),
            fs_version: u16::from_le_bytes([data[42], data[43]]),
            root_cluster: u32::from_le_bytes([data[44], data[45], data[46], data[47]]),
            fsinfo_sector: u16::from_le_bytes([data[48], data[49]]),
            backup_boot_sector: u16::from_le_bytes([data[50], data[51]]),
            reserved: [
                data[52], data[53], data[54], data[55], data[56], data[57], data[58], data[59],
                data[60], data[61], data[62], data[63],
            ],
            drive_number: data[64],
            winnt_flags: data[65],
            signature: data[66],
            volume_id: u32::from_le_bytes([data[67], data[68], data[69], data[70]]),
            volume_label: [
                data[71], data[72], data[73], data[74], data[75], data[76], data[77], data[78],
                data[79], data[80], data[81],
            ],
            fs_type: [
                data[82], data[83], data[84], data[85], data[86], data[87], data[88], data[89],
            ],
            bootcode: [0; 420],
            signature_word: u16::from_le_bytes([data[510], data[511]]),
            root_dir_sectors: 0,
        };

        serial_println!(
            "BootSector: bytes_per_sector={}, sectors_per_cluster={}, reserved_sectors={}, num_fats={}, fat_size_32={}, root_cluster={}",
            bs.bytes_per_sector,
            bs.sectors_per_cluster,
            bs.reserved_sectors,
            bs.num_fats,
            bs.fat_size_32,
            bs.root_cluster
        );

        if bs.fat_size_32 == 0 {
            return Err(FileSystemError::DiskOpError);
        }

        if bs.root_entries != 0 {
            return Err(FileSystemError::DiskOpError);
        }

        Ok(bs)
    }
}
