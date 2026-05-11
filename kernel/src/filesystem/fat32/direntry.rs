use crate::filesystem::fat32::{MAX_CLUSTER, ROOT_CLUSTER};
use crate::filesystem::sirius::FileType;
use alloc::string::String;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
pub enum FatFileAttributes {
    // TODO: can i use the reserved upper 2 bits for my stuff
    None = 0x00,
    ReadOnly = 0x01,  // reject changes to the file (write, delete, rename)
    Hidden = 0x02,    // listing should hide the file unless -a flag
    System = 0x04,    // indicates that its a system file (means nothing for now)
    VolumeId = 0x08, // indicates that this entry has the volume label of the volume, only one such entry can exist in the root directory
    Directory = 0x10, // indicates that this entry is a directory, otherwise its a file
    Archive = 0x20, // for backup utilities - Set by FAT driver on new creation, or when modifying/renaming the file. The backup utilities able to easily find the file to be backed up and it clears the attribute after the back up process
}

pub const FIRST_BYTE_IS_EMPTY_FLAG: u8 = 0x00;
pub const FIRST_BYTE_IS_DELETED_FLAG: u8 = 0xE5;

pub const MAX_NAME_LENGTH: usize = 8;
pub const MAX_EXT_LENGTH: usize = 3;
pub const MAX_FULL_NAME_LENGTH: usize = MAX_NAME_LENGTH + MAX_EXT_LENGTH;

pub const DIRECTORY_ENTRY_SIZE: usize = 32;

const THIS_DIR_ENTRY_NAME: [u8; 8] = *b".       ";
const THIS_DIR_ENTRY_EXT: [u8; 3] = *b"   ";
const PARENT_DIR_ENTRY_NAME: [u8; 8] = *b"..      ";
const PARENT_DIR_ENTRY_EXT: [u8; 3] = *b"   ";

// FAT32 Directory Entry (32 bytes)
// max size of a directory is 2MB (65536 entries)
// root directory is the top node of the hierarchy in a volume
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DirectoryEntry {
    pub name: [u8; 8],           // offset 0x00: Short filename
    pub extension: [u8; 3],      // offset 0x08: File extension
    pub attributes: u8,          // offset 0x0B: File attributes
    pub nt_reserved: u8,         // offset 0x0C: Reserved for NT
    pub creation_time_tenth: u8, // offset 0x0D: Tenths of second ([0,199] in unit of 10 ms)
    pub creation_time: u16,      // offset 0x0E: File creation time (HH:MM:SS)
    pub creation_date: u16,      // offset 0x10: File creation date (YYYY:MM:DD)
    pub access_date: u16,        // offset 0x12: Last access date
    pub first_cluster_high: u16, // offset 0x14: High word of the first cluster
    pub write_time: u16,         // offset 0x16: Write time
    pub write_date: u16,         // offset 0x18: Write date
    pub first_cluster_low: u16,  // offset 0x1A: Low word of the first cluster
    pub file_size: u32,          // offset 0x1C: File size in bytes
}

const fn is_leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

const DAYS_UNTIL_YEAR_SINCE_1970: [u32; 128] = {
    let mut days = [0; 128];
    let mut cumsum = 0;
    let mut y = 1970;
    while y < 1970 + 128 {
        let idx = (y - 1970) as usize;
        days[idx] = cumsum;
        cumsum += if is_leap_year(y) { 366 } else { 365 };
        y += 1;
    }
    days
};

// FAT date format:
// Seconds: 0-4b [0, 29] 2 second intervals
// Minutes: 5-10b [0, 59]
// Hours: 11-15b [0, 23]
// Day: 0-4b [1, 31]
// Month: 5-8b [1, 12]
// Year: 9-15b (0 == the year 1980)
pub fn fat_time_to_unix_timestamp(time: u16, date: u16) -> u32 {
    let seconds = ((time & 0x1F) as u32) * 2;
    let minutes = ((time >> 5) & 0x3F) as u32;
    let hours = ((time >> 11) & 0x1F) as u32;
    let day = (date & 0x1F) as u32;
    let month = ((date >> 5) & 0x0F) as u32;
    let year_idx = ((date >> 9) & 0x7F) as usize;

    let mut days = DAYS_UNTIL_YEAR_SINCE_1970[year_idx];
    let month_days = match month {
        1 => 0,
        2 => 31,
        3 => 59,
        4 => 90,
        5 => 120,
        6 => 151,
        7 => 181,
        8 => 212,
        9 => 243,
        10 => 273,
        11 => 304,
        12 => 334,
        _ => 0,
    };
    days += month_days;

    let year = year_idx as u32 + 1980;
    if month > 2 && is_leap_year(year) {
        days += 1;
    }

    if day > 1 {
        days += day - 1;
    }

    // convert to seconds
    (days * 86400) + (hours * 3600) + (minutes * 60) + seconds
}

impl DirectoryEntry {
    pub fn from_bytes(data: &[u8]) -> Self {
        assert!(data.len() >= 32);

        let first_cluster_high = u16::from_le_bytes([data[20], data[21]]);
        let first_cluster_low = u16::from_le_bytes([data[26], data[27]]);
        let cluster = (first_cluster_high as u32) << 16 | first_cluster_low as u32;

        assert!(cluster <= MAX_CLUSTER, "Cluster {} > MAX_CLUSTER", cluster);

        DirectoryEntry {
            name: [
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ],
            extension: [data[8], data[9], data[10]],
            attributes: data[11],
            nt_reserved: data[12],
            creation_time_tenth: data[13],
            creation_time: u16::from_le_bytes([data[14], data[15]]),
            creation_date: u16::from_le_bytes([data[16], data[17]]),
            access_date: u16::from_le_bytes([data[18], data[19]]),
            first_cluster_high,
            write_time: u16::from_le_bytes([data[22], data[23]]),
            write_date: u16::from_le_bytes([data[24], data[25]]),
            first_cluster_low,
            file_size: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
        }
    }

    pub fn create_empty() -> Self {
        DirectoryEntry {
            name: [0u8; MAX_NAME_LENGTH],
            extension: [0u8; MAX_EXT_LENGTH],
            attributes: FatFileAttributes::None as u8,
            nt_reserved: 0,
            creation_time_tenth: 0,
            creation_time: 0,
            creation_date: 0,
            access_date: 0,
            first_cluster_high: 0,
            write_time: 0,
            write_date: 0,
            first_cluster_low: 0,
            file_size: 0,
        }
    }

    pub fn create_dot_entry(cluster: u32) -> Self {
        let mut entry = DirectoryEntry::create_empty();
        entry.name.copy_from_slice(&THIS_DIR_ENTRY_NAME);
        entry.attributes = FatFileAttributes::Directory as u8;
        entry.set_first_cluster(cluster);
        entry
    }

    pub fn create_dot_dot_entry(parent_cluster: u32) -> Self {
        let mut entry = DirectoryEntry::create_empty();
        entry.name.copy_from_slice(&PARENT_DIR_ENTRY_NAME);
        entry.attributes = FatFileAttributes::Directory as u8;
        entry.set_first_cluster(parent_cluster);
        entry
    }

    pub fn is_valid(&self) -> bool {
        if self.name[0] == FIRST_BYTE_IS_EMPTY_FLAG || self.name[0] == FIRST_BYTE_IS_DELETED_FLAG {
            return false;
        }
        true
    }

    pub fn is_deleted(&self) -> bool {
        self.name[0] == FIRST_BYTE_IS_DELETED_FLAG
    }

    pub fn is_empty(&self) -> bool {
        self.name[0] == FIRST_BYTE_IS_EMPTY_FLAG
    }

    pub fn is_directory(&self) -> bool {
        (self.attributes & FatFileAttributes::Directory as u8) != 0
    }

    pub fn is_volume_id(&self) -> bool {
        (self.attributes & FatFileAttributes::VolumeId as u8) != 0
    }

    pub fn is_current_directory(&self) -> bool {
        self.name == THIS_DIR_ENTRY_NAME
            && self.extension == THIS_DIR_ENTRY_EXT
            && self.is_directory()
    }

    pub fn full_name(&self) -> [u8; MAX_FULL_NAME_LENGTH] {
        let mut full_name = [0u8; MAX_FULL_NAME_LENGTH];
        full_name[..8].copy_from_slice(&self.name);
        full_name[8..].copy_from_slice(&self.extension);
        full_name
    }

    pub fn is_parent_directory(&self) -> bool {
        self.name == PARENT_DIR_ENTRY_NAME
            && self.extension == PARENT_DIR_ENTRY_EXT
            && self.is_directory()
    }

    pub fn get_file_type(&self) -> FileType {
        if self.is_directory() {
            FileType::Directory
        } else {
            FileType::File
        }
    }

    pub fn get_first_cluster(&self) -> u32 {
        ((self.first_cluster_high as u32) << 16) | (self.first_cluster_low as u32)
    }

    pub fn set_first_cluster(&mut self, cluster: u32) {
        self.first_cluster_high = (cluster >> 16) as u16;
        self.first_cluster_low = cluster as u16;
    }

    // the filename without extension
    pub fn get_stem(&self) -> String {
        let length = self
            .name
            .iter()
            .take_while(|&&c| c != 0 && c != 0x20)
            .count();
        let stem = unsafe { String::from_utf8_unchecked(self.name[..length].to_vec()) };
        //let stem = core::str::from_utf8(&self.name[..length])
        //    .unwrap_or("notutf8")
        //    .to_string();
        stem
    }

    // the filename with extension
    pub fn get_filename(&self) -> String {
        let mut name = self.get_stem();

        let ext_length = self
            .extension
            .iter()
            .take_while(|&&c| c != 0 && c != 0x20)
            .count();

        if ext_length > 0 {
            name.push('.');
            name.push_str(core::str::from_utf8(&self.extension[..ext_length]).unwrap_or("???"));
            name
        } else {
            name
        }
    }

    pub fn full_name_matches(&self, fullname: &[u8; MAX_FULL_NAME_LENGTH]) -> bool {
        self.name == fullname[0..8] && self.extension == fullname[8..11]
    }

    pub fn set_filename(&mut self, filename: &str) {
        self.name.fill(b' ');
        self.extension.fill(b' ');

        let last_dot = filename.rfind('.');

        let (name_part, ext_part) = match last_dot {
            Some(pos) => {
                let name = &filename[..pos];
                let ext = &filename[pos + 1..];
                (name, Some(ext))
            }
            None => (filename, None),
        };

        let name_bytes = name_part.as_bytes();
        for (i, &c) in name_bytes.iter().enumerate().take(8) {
            self.name[i] = match c {
                b'a'..=b'z' => c - 32,
                _ => c,
            };
        }

        if let Some(ext) = ext_part {
            let ext_bytes = ext.as_bytes();
            for (i, &c) in ext_bytes.iter().enumerate().take(3) {
                self.extension[i] = match c {
                    b'a'..=b'z' => c - 32,
                    _ => c,
                };
            }
        }
    }

    pub fn mark_deleted(&mut self) {
        self.name[0] = FIRST_BYTE_IS_DELETED_FLAG;
    }

    pub fn get_creation_timestamp(&self) -> u32 {
        fat_time_to_unix_timestamp(self.creation_time, self.creation_date)
    }

    pub fn get_modified_timestamp(&self) -> u32 {
        fat_time_to_unix_timestamp(self.write_time, self.write_date)
    }

    pub fn as_bytes(&self) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[0..8].copy_from_slice(&self.name);
        bytes[8..11].copy_from_slice(&self.extension);
        bytes[11] = self.attributes;
        bytes[12] = self.nt_reserved;
        bytes[13] = self.creation_time_tenth;
        bytes[14..16].copy_from_slice(&self.creation_time.to_le_bytes());
        bytes[16..18].copy_from_slice(&self.creation_date.to_le_bytes());
        bytes[18..20].copy_from_slice(&self.access_date.to_le_bytes());
        bytes[20..22].copy_from_slice(&self.first_cluster_high.to_le_bytes());
        bytes[22..24].copy_from_slice(&self.write_time.to_le_bytes());
        bytes[24..26].copy_from_slice(&self.write_date.to_le_bytes());
        bytes[26..28].copy_from_slice(&self.first_cluster_low.to_le_bytes());
        bytes[28..32].copy_from_slice(&self.file_size.to_le_bytes());
        bytes
    }

    // NOTE: this is a fake directory entry
    pub fn create_for_root() -> Self {
        DirectoryEntry {
            name: *b"        ",
            extension: *b"   ",
            attributes: FatFileAttributes::Directory as u8,
            nt_reserved: 0,
            creation_time_tenth: 0,
            creation_time: 0,
            creation_date: 0,
            access_date: 0,
            first_cluster_high: (ROOT_CLUSTER >> 16) as u16,
            write_time: 0,
            write_date: 0,
            first_cluster_low: ROOT_CLUSTER as u16,
            file_size: 0,
        }
    }
}
