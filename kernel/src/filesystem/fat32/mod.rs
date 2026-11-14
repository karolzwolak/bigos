pub mod boot_sector;
pub mod direntry;
pub mod test_data;

use crate::filesystem::fat32::direntry::{FatFileAttributes, MAX_EXT_LENGTH, MAX_NAME_LENGTH};
use crate::filesystem::vfs::{
    FileAttributes, FileNode, FileSystemError, FileSystemResult, FileType,
};
use crate::io::disk::{BlockDevice, SECTOR_SIZE_BYTES};
use crate::serial_println;
use alloc::string::String;
use alloc::vec::Vec;
use boot_sector::BootSector;
use core::cmp::{max, min};
use direntry::{DIRECTORY_ENTRY_SIZE, DirectoryEntry, MAX_FULL_NAME_LENGTH};

pub const ROOT_CLUSTER: u32 = 2;
// node_id packing assumes up to 2^24 clusters
pub const MAX_CLUSTER: u32 = 0xFF_FFFF;

/// placeholder for "no cluster", used for end of chain and empty files
pub const NO_CLUSTER: u32 = 0;

const FAT_ENTRY_SIZE_BYTES: usize = 4;
const FAT_ENTRY_MASK: u32 = 0x0FFF_FFFF;
const FAT_ENTRY_FREE: u32 = 0;
pub const END_OF_CHAIN: u32 = 0x0FFF_FFFF;
pub const BAD_CLUSTER: u32 = 0x0FFF_FFF7;
pub const FAT_ENTRY_RESERVED_BEGIN: u32 = 0x0FFF_FFF8;
pub const FAT_ENTRY_RESERVED_END: u32 = 0x0FFF_FFFE;

const MAX_FILE_SIZE_BYTES: usize = u32::MAX as usize;

// node handle layout: bit 63 by-entry-index flag, bits 56..63 fat attributes,
// bits 32..56 parent directory cluster, bits 0..24 first cluster or entry index
const NODE_BY_ENTRY_INDEX_FLAG: usize = 1 << 63;
const NODE_ATTRIBUTES_SHIFT: u32 = 56;
const NODE_ATTRIBUTES_MASK: usize = 0x7F;
const NODE_PARENT_CLUSTER_SHIFT: u32 = 32;
const NODE_CLUSTER_MASK: usize = MAX_CLUSTER as usize;

pub type FileNodeHandle = usize;
pub const INVALID_NODE_HANDLE: FileNodeHandle = 0;

const fn is_directory(fat_attributes: u8) -> bool {
    fat_attributes & FatFileAttributes::Directory as u8 != 0
}

fn is_valid_short_name_char(character: char) -> bool {
    character.is_ascii_graphic()
        && !matches!(
            character,
            '"' | '*'
                | '+'
                | ','
                | '/'
                | ':'
                | ';'
                | '<'
                | '='
                | '>'
                | '?'
                | '['
                | '\\'
                | ']'
                | '|'
        )
}

fn is_valid_filename(name: &str) -> bool {
    if name.is_empty()
        || name.starts_with('.')
        || !name
            .chars()
            .all(|c| c == '.' || is_valid_short_name_char(c))
    {
        return false;
    }

    match name.rfind('.') {
        Some(dot) => {
            let stem = &name[..dot];
            let extension_length = name.len() - dot - 1;

            stem.len() <= MAX_NAME_LENGTH
                && !stem.contains('.')
                && extension_length > 0
                && extension_length <= MAX_EXT_LENGTH
        }
        None => name.len() <= MAX_NAME_LENGTH,
    }
}

/// result of unpacking a FileNodeHandle
struct DecodedNode {
    /// first cluster of the node, or the entry index in the parent when by_entry_index is set
    cluster_or_entry_index: u32,
    parent_cluster: u32,
    attributes: u8,
    by_entry_index: bool,
}

fn encode_node_id(
    entry: &DirectoryEntry,
    parent_cluster: u32,
    entry_index: usize,
) -> FileNodeHandle {
    let first_cluster = entry.get_first_cluster();
    // an empty file has no cluster to identify it, so identify it by its slot in the parent
    // a cluster that does not fit the handle comes from a corrupt entry, it is identified by
    // its slot too and rejected when the entry is loaded
    let (cluster_or_entry_index, by_entry_index_flag) =
        if first_cluster == NO_CLUSTER || first_cluster > MAX_CLUSTER {
            (entry_index, NODE_BY_ENTRY_INDEX_FLAG)
        } else {
            (first_cluster as usize, 0)
        };
    debug_assert!(cluster_or_entry_index <= NODE_CLUSTER_MASK);
    debug_assert!(parent_cluster as usize <= NODE_CLUSTER_MASK);

    by_entry_index_flag
        | ((entry.attributes as usize & NODE_ATTRIBUTES_MASK) << NODE_ATTRIBUTES_SHIFT)
        | ((parent_cluster as usize & NODE_CLUSTER_MASK) << NODE_PARENT_CLUSTER_SHIFT)
        | (cluster_or_entry_index & NODE_CLUSTER_MASK)
}

fn decode_node_id(node_id: FileNodeHandle) -> DecodedNode {
    DecodedNode {
        cluster_or_entry_index: (node_id & NODE_CLUSTER_MASK) as u32,
        parent_cluster: ((node_id >> NODE_PARENT_CLUSTER_SHIFT) & NODE_CLUSTER_MASK) as u32,
        attributes: ((node_id >> NODE_ATTRIBUTES_SHIFT) & NODE_ATTRIBUTES_MASK) as u8,
        by_entry_index: node_id & NODE_BY_ENTRY_INDEX_FLAG != 0,
    }
}

fn to_fat32_name(name: &str) -> FileSystemResult<[u8; MAX_FULL_NAME_LENGTH]> {
    if !is_valid_filename(name) {
        return Err(FileSystemError::InvalidFilename);
    }

    let mut fat32_name = [b' '; MAX_FULL_NAME_LENGTH];

    let (stem, extension) = match name.rfind('.') {
        Some(dot_pos) => (&name[..dot_pos], &name[dot_pos + 1..]),
        None => (name, ""),
    };

    for (i, c) in stem.bytes().enumerate() {
        fat32_name[i] = c.to_ascii_uppercase();
    }
    for (i, c) in extension.bytes().enumerate() {
        fat32_name[MAX_NAME_LENGTH + i] = c.to_ascii_uppercase();
    }

    Ok(fat32_name)
}

const fn file_attributes_from_fat_attributes(fat_attributes: u8) -> FileAttributes {
    if is_directory(fat_attributes) {
        FileAttributes::DIR_DEFAULT
    } else if fat_attributes & FatFileAttributes::ReadOnly as u8 == 0 {
        FileAttributes::FILE_READ_WRITE
    } else {
        FileAttributes::FILE_READONLY
    }
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn node_from_entry(entry: &DirectoryEntry, parent_cluster: u32, entry_index: usize) -> FileNode {
    FileNode {
        node_id: encode_node_id(entry, parent_cluster, entry_index),
        name: entry.get_filename(),
        file_type: entry.get_file_type(),
        size: entry.file_size as usize,
        created_time: entry.get_creation_timestamp(),
        modified_time: entry.get_modified_timestamp(),
        attributes: file_attributes_from_fat_attributes(entry.attributes),
    }
}

pub struct Fat32Driver {
    pub device: BlockDevice,
    pub boot_sector: BootSector,
    pub fat_start_sector: u64,
    pub data_start_sector: u64,
    pub sectors_per_cluster: u32,
    pub cluster_size: usize,
    /// exclusive upper bound of valid cluster numbers
    pub max_cluster: u32,
    pub root_cluster: u32,

    /// search for free clusters starts here
    next_free_hint: u32,
    /// scratch space for one cluster, owned for the lifetime of the mount
    cluster_buffer: Vec<u8>,

    root_dir_node_id: FileNodeHandle,
    root_direntry: DirectoryEntry,
    root_filenode: FileNode,
}

impl Fat32Driver {
    pub fn new(mut device: BlockDevice) -> FileSystemResult<Self> {
        let mut boot_sector_data = [0u8; SECTOR_SIZE_BYTES];
        device.read_sectors(0, &mut boot_sector_data)?;

        let boot_sector = BootSector::from_bytes(&boot_sector_data)?;
        if boot_sector.bytes_per_sector as usize != SECTOR_SIZE_BYTES
            || boot_sector.num_fats == 0
            || !boot_sector.sectors_per_cluster.is_power_of_two()
        {
            return Err(FileSystemError::NotSupported);
        }

        let fat_start_sector = boot_sector.reserved_sectors as u64;
        let sectors_per_cluster = boot_sector.sectors_per_cluster as u32;
        let fat_size_sectors = boot_sector.fat_size_32 as u64;
        let data_start_sector = fat_start_sector + (boot_sector.num_fats as u64 * fat_size_sectors);
        let cluster_size = (sectors_per_cluster as usize) * SECTOR_SIZE_BYTES;

        // total_sectors_32 is 0 on small volumes, which use total_sectors_16
        let total_sectors = if boot_sector.total_sectors_32 != 0 {
            boot_sector.total_sectors_32 as u64
        } else {
            boot_sector.total_sectors_16 as u64
        };
        if total_sectors <= data_start_sector {
            return Err(FileSystemError::IoError);
        }

        let total_clusters = (total_sectors - data_start_sector) / sectors_per_cluster as u64;
        let clusters_described_by_fat =
            fat_size_sectors * (SECTOR_SIZE_BYTES / FAT_ENTRY_SIZE_BYTES) as u64;
        let max_cluster = min(
            total_clusters + ROOT_CLUSTER as u64,
            clusters_described_by_fat,
        );
        if max_cluster > MAX_CLUSTER as u64 {
            return Err(FileSystemError::NotSupported);
        }

        let root_cluster = boot_sector.root_cluster;
        debug_assert!(root_cluster == ROOT_CLUSTER);

        let root_direntry = DirectoryEntry::create_for_root();
        let root_dir_node_id = encode_node_id(&root_direntry, root_cluster, 0);
        let root_filenode = FileNode {
            node_id: root_dir_node_id,
            name: String::from("/"),
            file_type: FileType::Directory,
            size: 0,
            created_time: 0,
            modified_time: 0,
            attributes: FileAttributes::DIR_DEFAULT,
        };

        serial_println!(
            "Fat32Driver: mounted, fat_start_sector={}, fat_size_sectors={}, data_start_sector={}, cluster_size={}, max_cluster={}",
            fat_start_sector,
            fat_size_sectors,
            data_start_sector,
            cluster_size,
            max_cluster
        );

        Ok(Self {
            device,
            boot_sector,
            fat_start_sector,
            data_start_sector,
            sectors_per_cluster,
            cluster_size,
            max_cluster: max_cluster as u32,
            root_cluster,
            next_free_hint: ROOT_CLUSTER,
            cluster_buffer: alloc::vec![0u8; cluster_size],
            root_dir_node_id,
            root_direntry,
            root_filenode,
        })
    }

    pub fn root_node(&self) -> FileNodeHandle {
        self.root_dir_node_id
    }

    /// whether a first cluster read from a directory entry may be followed, NO_CLUSTER means empty
    fn is_cluster_followable(&self, cluster: u32) -> bool {
        cluster == NO_CLUSTER || (ROOT_CLUSTER..self.max_cluster).contains(&cluster)
    }

    fn cluster_to_sector(&self, cluster: u32) -> u64 {
        debug_assert!(cluster >= ROOT_CLUSTER && cluster < self.max_cluster);

        self.data_start_sector + ((cluster - ROOT_CLUSTER) as u64 * self.sectors_per_cluster as u64)
    }

    /// sector of the first FAT copy that holds the entry, and the offset of the entry in it
    fn fat_entry_location(&self, cluster: u32) -> (u64, usize) {
        debug_assert!(cluster < self.max_cluster);

        let fat_offset_bytes = cluster as u64 * FAT_ENTRY_SIZE_BYTES as u64;
        let sector = self.fat_start_sector + fat_offset_bytes / SECTOR_SIZE_BYTES as u64;

        (
            sector,
            (fat_offset_bytes % SECTOR_SIZE_BYTES as u64) as usize,
        )
    }

    fn read_cluster_into_buffer(&mut self, cluster: u32) -> FileSystemResult<()> {
        let sector = self.cluster_to_sector(cluster);
        self.device.read_sectors(sector, &mut self.cluster_buffer)?;

        Ok(())
    }

    fn write_cluster_from_buffer(&mut self, cluster: u32) -> FileSystemResult<()> {
        let sector = self.cluster_to_sector(cluster);
        self.device.write_sectors(sector, &self.cluster_buffer)?;

        Ok(())
    }

    fn read_fat_entry(&mut self, cluster: u32) -> FileSystemResult<u32> {
        let (sector, offset_in_sector) = self.fat_entry_location(cluster);
        let mut sector_buffer = [0u8; SECTOR_SIZE_BYTES];
        self.device.read_sectors(sector, &mut sector_buffer)?;

        Ok(read_u32_le(&sector_buffer, offset_in_sector) & FAT_ENTRY_MASK)
    }

    /// writes the entry into every FAT copy, the top 4 bits of the entry are preserved
    fn write_fat_entry(&mut self, cluster: u32, value: u32) -> FileSystemResult<()> {
        debug_assert!(value & !FAT_ENTRY_MASK == 0);

        let (first_fat_sector, offset_in_sector) = self.fat_entry_location(cluster);
        let mut sector_buffer = [0u8; SECTOR_SIZE_BYTES];

        for fat_index in 0..self.boot_sector.num_fats as u64 {
            let sector = first_fat_sector + fat_index * self.boot_sector.fat_size_32 as u64;
            self.device.read_sectors(sector, &mut sector_buffer)?;

            let old_value = read_u32_le(&sector_buffer, offset_in_sector);
            let new_value = (old_value & !FAT_ENTRY_MASK) | value;
            sector_buffer[offset_in_sector..offset_in_sector + FAT_ENTRY_SIZE_BYTES]
                .copy_from_slice(&new_value.to_le_bytes());

            self.device.write_sectors(sector, &sector_buffer)?;
        }

        Ok(())
    }

    /// next cluster in the chain, or NO_CLUSTER at the end of the chain
    fn get_next_cluster(&mut self, cluster: u32) -> FileSystemResult<u32> {
        match self.read_fat_entry(cluster)? {
            FAT_ENTRY_RESERVED_BEGIN..=END_OF_CHAIN => Ok(NO_CLUSTER),
            next if next >= ROOT_CLUSTER && next < self.max_cluster => Ok(next),
            _ => Err(FileSystemError::IoError),
        }
    }

    /// walks hop_count links from first_cluster, NO_CLUSTER if the chain is shorter
    fn nth_cluster(&mut self, first_cluster: u32, hop_count: usize) -> FileSystemResult<u32> {
        let mut cluster = first_cluster;
        for _ in 0..hop_count {
            cluster = self.get_next_cluster(cluster)?;
            if cluster == NO_CLUSTER {
                break;
            }
        }

        Ok(cluster)
    }

    fn get_cluster_chain_length(&mut self, first_cluster: u32) -> FileSystemResult<usize> {
        let mut length = 0;
        let mut cluster = first_cluster;
        while cluster != NO_CLUSTER {
            length += 1;
            cluster = self.get_next_cluster(cluster)?;
        }

        Ok(length)
    }

    fn last_cluster_of_chain(&mut self, first_cluster: u32) -> FileSystemResult<u32> {
        let mut cluster = first_cluster;
        loop {
            let next = self.get_next_cluster(cluster)?;
            if next == NO_CLUSTER {
                return Ok(cluster);
            }
            cluster = next;
        }
    }

    /// returns the number of clusters in a run of consecutive cluster numbers starting at
    /// first_cluster (at most max_run_length), and the cluster that follows the run in the chain
    /// when want_next_cluster is set
    fn contiguous_run(
        &mut self,
        first_cluster: u32,
        max_run_length: usize,
        want_next_cluster: bool,
    ) -> FileSystemResult<(usize, u32)> {
        debug_assert!(max_run_length >= 1);

        let mut run_length = 1;
        let mut last_cluster = first_cluster;

        while run_length < max_run_length {
            let next = self.get_next_cluster(last_cluster)?;
            if next != last_cluster + 1 {
                return Ok((run_length, next));
            }
            last_cluster = next;
            run_length += 1;
        }

        let next = if want_next_cluster {
            self.get_next_cluster(last_cluster)?
        } else {
            NO_CLUSTER
        };

        Ok((run_length, next))
    }

    fn scan_for_free_cluster(
        &mut self,
        from_cluster: u32,
        to_cluster: u32,
    ) -> FileSystemResult<u32> {
        let mut sector_buffer = [0u8; SECTOR_SIZE_BYTES];
        let mut loaded_sector = u64::MAX;

        for cluster in from_cluster..to_cluster {
            let (sector, offset_in_sector) = self.fat_entry_location(cluster);
            if sector != loaded_sector {
                self.device.read_sectors(sector, &mut sector_buffer)?;
                loaded_sector = sector;
            }

            if read_u32_le(&sector_buffer, offset_in_sector) & FAT_ENTRY_MASK == FAT_ENTRY_FREE {
                return Ok(cluster);
            }
        }

        Ok(NO_CLUSTER)
    }

    fn find_free_cluster(&mut self) -> FileSystemResult<u32> {
        let hint = self.next_free_hint.clamp(ROOT_CLUSTER, self.max_cluster);

        let mut free_cluster = self.scan_for_free_cluster(hint, self.max_cluster)?;
        if free_cluster == NO_CLUSTER {
            free_cluster = self.scan_for_free_cluster(ROOT_CLUSTER, hint)?;
        }
        if free_cluster == NO_CLUSTER {
            return Err(FileSystemError::NoSpace);
        }

        Ok(free_cluster)
    }

    /// allocates one zeroed cluster as the new end of a chain, linked after previous_cluster
    /// unless that is NO_CLUSTER. Leaves cluster_buffer zeroed
    fn allocate_cluster(&mut self, previous_cluster: u32) -> FileSystemResult<u32> {
        let new_cluster = self.find_free_cluster()?;

        self.cluster_buffer.fill(0);
        self.write_cluster_from_buffer(new_cluster)?;
        self.write_fat_entry(new_cluster, END_OF_CHAIN)?;
        if previous_cluster != NO_CLUSTER {
            self.write_fat_entry(previous_cluster, new_cluster)?;
        }
        self.next_free_hint = new_cluster + 1;

        Ok(new_cluster)
    }

    fn free_cluster_chain(&mut self, first_cluster: u32) -> FileSystemResult<()> {
        let mut cluster = first_cluster;
        while cluster != NO_CLUSTER {
            let next = self.get_next_cluster(cluster)?;
            self.write_fat_entry(cluster, FAT_ENTRY_FREE)?;
            self.next_free_hint = min(self.next_free_hint, cluster);
            cluster = next;
        }

        Ok(())
    }

    /// returns all entries up to the end marker, including deleted ones and the volume label,
    /// since the index of an entry in the returned vector identifies its slot in the directory
    fn read_directory_entries(
        &mut self,
        first_cluster: u32,
    ) -> FileSystemResult<Vec<DirectoryEntry>> {
        let mut entries = Vec::new();
        let mut cluster = first_cluster;

        while cluster != NO_CLUSTER {
            self.read_cluster_into_buffer(cluster)?;

            for entry_bytes in self
                .cluster_buffer
                .as_chunks::<DIRECTORY_ENTRY_SIZE>()
                .0
                .iter()
            {
                let entry = DirectoryEntry::from_bytes(entry_bytes);
                if entry.is_empty() {
                    return Ok(entries);
                }
                entries.push(entry);
            }

            cluster = self.get_next_cluster(cluster)?;
        }

        Ok(entries)
    }

    fn read_direntry_at(
        &mut self,
        dir_cluster: u32,
        entry_index: usize,
    ) -> FileSystemResult<DirectoryEntry> {
        let entries_per_cluster = self.cluster_size / DIRECTORY_ENTRY_SIZE;
        let cluster = self.nth_cluster(dir_cluster, entry_index / entries_per_cluster)?;
        if cluster == NO_CLUSTER {
            return Err(FileSystemError::NotFound);
        }

        self.read_cluster_into_buffer(cluster)?;
        let offset = (entry_index % entries_per_cluster) * DIRECTORY_ENTRY_SIZE;

        Ok(DirectoryEntry::from_bytes(
            &self.cluster_buffer[offset..offset + DIRECTORY_ENTRY_SIZE],
        ))
    }

    /// read-modify-write of the single directory cluster that holds the entry
    fn write_direntry(
        &mut self,
        dir_cluster: u32,
        entry_index: usize,
        entry: &DirectoryEntry,
    ) -> FileSystemResult<()> {
        let entries_per_cluster = self.cluster_size / DIRECTORY_ENTRY_SIZE;
        let cluster = self.nth_cluster(dir_cluster, entry_index / entries_per_cluster)?;
        if cluster == NO_CLUSTER {
            return Err(FileSystemError::NoSpace);
        }

        self.read_cluster_into_buffer(cluster)?;
        let offset = (entry_index % entries_per_cluster) * DIRECTORY_ENTRY_SIZE;
        self.cluster_buffer[offset..offset + DIRECTORY_ENTRY_SIZE]
            .copy_from_slice(&entry.as_bytes());

        self.write_cluster_from_buffer(cluster)
    }

    /// slot for a new entry: a deleted one, the end marker, or the first slot of a new cluster
    fn find_free_slot_in_directory(
        &mut self,
        dir_cluster: u32,
        entries: &[DirectoryEntry],
    ) -> FileSystemResult<usize> {
        if let Some(deleted_slot) = entries.iter().position(|e| !e.is_valid()) {
            return Ok(deleted_slot);
        }

        let entries_per_cluster = self.cluster_size / DIRECTORY_ENTRY_SIZE;
        let capacity = self.get_cluster_chain_length(dir_cluster)? * entries_per_cluster;
        if entries.len() < capacity {
            return Ok(entries.len());
        }

        let last_cluster = self.last_cluster_of_chain(dir_cluster)?;
        self.allocate_cluster(last_cluster)?;

        Ok(entries.len())
    }

    fn find_direntry(&mut self, path: &str) -> FileSystemResult<(DirectoryEntry, u32, usize)> {
        let mut found = (self.root_direntry, self.root_cluster, 0);
        let mut curr_cluster = self.root_cluster;
        let mut parts = path.split('/').filter(|p| !p.is_empty()).peekable();

        while let Some(part) = parts.next() {
            let fat32_part = to_fat32_name(part)?;
            let entries = self.read_directory_entries(curr_cluster)?;
            let entry_index = entries
                .iter()
                .position(|e| e.is_short_name_node() && e.full_name_matches(&fat32_part))
                .ok_or(FileSystemError::NotFound)?;
            let entry = entries[entry_index];
            if !self.is_cluster_followable(entry.get_first_cluster()) {
                return Err(FileSystemError::IoError);
            }

            if parts.peek().is_some() && !entry.is_directory() {
                return Err(FileSystemError::NotDirectory);
            }

            found = (entry, curr_cluster, entry_index);
            curr_cluster = entry.get_first_cluster();
        }

        Ok(found)
    }

    /// finds the directory entry a handle refers to, and its slot index in the parent
    fn load_entry(&mut self, node_id: FileNodeHandle) -> FileSystemResult<(DirectoryEntry, usize)> {
        let node = decode_node_id(node_id);

        if node.by_entry_index {
            let entry_index = node.cluster_or_entry_index as usize;
            let entry = self.read_direntry_at(node.parent_cluster, entry_index)?;
            if !entry.is_short_name_node() {
                return Err(FileSystemError::NotFound);
            }
            if !self.is_cluster_followable(entry.get_first_cluster()) {
                return Err(FileSystemError::IoError);
            }

            return Ok((entry, entry_index));
        }

        let entries = self.read_directory_entries(node.parent_cluster)?;
        let entry_index = entries
            .iter()
            .position(|e| {
                e.is_short_name_node() && e.get_first_cluster() == node.cluster_or_entry_index
            })
            .ok_or(FileSystemError::NotFound)?;

        Ok((entries[entry_index], entry_index))
    }

    fn is_root_node(&self, node_id: FileNodeHandle) -> bool {
        let node = decode_node_id(node_id);

        !node.by_entry_index && node.cluster_or_entry_index == self.root_cluster
    }

    pub fn read_file(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        out_buffer: &mut [u8],
    ) -> FileSystemResult<usize> {
        if is_directory(decode_node_id(node_id).attributes) {
            return Err(FileSystemError::IsDirectory);
        }

        let (entry, _) = self.load_entry(node_id)?;
        let file_size = entry.file_size as usize;
        let first_cluster = entry.get_first_cluster();
        if offset >= file_size || first_cluster == NO_CLUSTER || out_buffer.is_empty() {
            return Ok(0);
        }

        let bytes_to_read = min(out_buffer.len(), file_size - offset);
        let mut cluster = self.nth_cluster(first_cluster, offset / self.cluster_size)?;
        let mut offset_in_cluster = offset % self.cluster_size;
        let mut total_read = 0;

        while total_read < bytes_to_read && cluster != NO_CLUSTER {
            let remaining = bytes_to_read - total_read;

            if offset_in_cluster == 0 && remaining >= self.cluster_size {
                // whole clusters go straight into the caller buffer, one request per contiguous run
                let max_run_length = remaining / self.cluster_size;
                let is_tail_left = !remaining.is_multiple_of(self.cluster_size);
                let (run_length, next_cluster) =
                    self.contiguous_run(cluster, max_run_length, is_tail_left)?;
                let run_bytes = run_length * self.cluster_size;

                let sector = self.cluster_to_sector(cluster);
                self.device
                    .read_sectors(sector, &mut out_buffer[total_read..total_read + run_bytes])?;

                total_read += run_bytes;
                cluster = next_cluster;
            } else {
                // partial first or last cluster goes through the bounce buffer
                let chunk = min(self.cluster_size - offset_in_cluster, remaining);
                self.read_cluster_into_buffer(cluster)?;
                out_buffer[total_read..total_read + chunk].copy_from_slice(
                    &self.cluster_buffer[offset_in_cluster..offset_in_cluster + chunk],
                );

                total_read += chunk;
                offset_in_cluster = 0;
                if total_read < bytes_to_read {
                    cluster = self.get_next_cluster(cluster)?;
                }
            }
        }

        Ok(total_read)
    }

    /// offset may be at most the current file size. Returns the number of bytes written, which
    /// is smaller than data.len() only when the volume ran out of space
    pub fn write_file(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        data: &[u8],
    ) -> FileSystemResult<usize> {
        if is_directory(decode_node_id(node_id).attributes) {
            return Err(FileSystemError::IsDirectory);
        }
        if data.is_empty() {
            return Ok(0);
        }

        let parent_cluster = decode_node_id(node_id).parent_cluster;
        let (mut entry, entry_index) = self.load_entry(node_id)?;
        if entry.attributes & FatFileAttributes::ReadOnly as u8 != 0 {
            return Err(FileSystemError::PermissionDenied);
        }

        let old_size = entry.file_size as usize;
        if offset > old_size {
            return Err(FileSystemError::OffsetPastEnd);
        }
        if offset + data.len() > MAX_FILE_SIZE_BYTES {
            return Err(FileSystemError::FileSizeExceeded);
        }

        let mut cluster = entry.get_first_cluster();
        if cluster == NO_CLUSTER {
            cluster = self.allocate_cluster(NO_CLUSTER)?;
            entry.set_first_cluster(cluster);
            self.write_direntry(parent_cluster, entry_index, &entry)?;
        }

        // walk to the cluster that holds offset, extending the chain when offset is at its end
        for _ in 0..offset / self.cluster_size {
            let next = self.get_next_cluster(cluster)?;
            cluster = if next != NO_CLUSTER {
                next
            } else {
                self.allocate_cluster(cluster)?
            };
        }

        let mut offset_in_cluster = offset % self.cluster_size;
        let mut total_written = 0;

        while total_written < data.len() {
            let chunk = min(
                self.cluster_size - offset_in_cluster,
                data.len() - total_written,
            );
            let chunk_data = &data[total_written..total_written + chunk];

            if chunk == self.cluster_size {
                let sector = self.cluster_to_sector(cluster);
                self.device.write_sectors(sector, chunk_data)?;
            } else {
                self.read_cluster_into_buffer(cluster)?;
                self.cluster_buffer[offset_in_cluster..offset_in_cluster + chunk]
                    .copy_from_slice(chunk_data);
                self.write_cluster_from_buffer(cluster)?;
            }

            total_written += chunk;
            offset_in_cluster = 0;
            if total_written == data.len() {
                break;
            }

            let next = self.get_next_cluster(cluster)?;
            if next != NO_CLUSTER {
                cluster = next;
                continue;
            }
            match self.allocate_cluster(cluster) {
                Ok(new_cluster) => cluster = new_cluster,
                // short write, the data written so far is kept
                Err(FileSystemError::NoSpace) => break,
                Err(error) => return Err(error),
            }
        }

        let new_size = max(old_size, offset + total_written);
        if new_size != old_size {
            entry.file_size = new_size as u32;
            self.write_direntry(parent_cluster, entry_index, &entry)?;
        }

        Ok(total_written)
    }

    pub fn get_node(&mut self, node_id: FileNodeHandle) -> FileSystemResult<FileNode> {
        if self.is_root_node(node_id) {
            return Ok(self.root_filenode.clone());
        }

        let (entry, entry_index) = self.load_entry(node_id)?;

        Ok(node_from_entry(
            &entry,
            decode_node_id(node_id).parent_cluster,
            entry_index,
        ))
    }

    pub fn list_directory(&mut self, node_id: FileNodeHandle) -> FileSystemResult<Vec<FileNode>> {
        let node = decode_node_id(node_id);
        if !is_directory(node.attributes) {
            return Err(FileSystemError::NotDirectory);
        }
        if node.by_entry_index {
            // a directory with a corrupt first cluster
            return Err(FileSystemError::IoError);
        }

        let dir_cluster = node.cluster_or_entry_index;
        let entries = self.read_directory_entries(dir_cluster)?;
        let mut nodes = Vec::with_capacity(entries.len());

        for (entry_index, entry) in entries.iter().enumerate() {
            if !entry.is_short_name_node()
                || entry.is_current_directory()
                || entry.is_parent_directory()
            {
                continue;
            }

            nodes.push(node_from_entry(entry, dir_cluster, entry_index));
        }

        Ok(nodes)
    }

    pub fn find_node(&mut self, path: &str) -> FileSystemResult<FileNodeHandle> {
        let (entry, parent_cluster, entry_index) = self.find_direntry(path)?;
        debug_assert!(!(entry.is_directory() && entry.file_size != 0));

        Ok(encode_node_id(&entry, parent_cluster, entry_index))
    }

    fn create_node(
        &mut self,
        parent_id: FileNodeHandle,
        name: &str,
        is_new_node_directory: bool,
    ) -> FileSystemResult<FileNodeHandle> {
        let parent = decode_node_id(parent_id);
        if !is_directory(parent.attributes) {
            return Err(FileSystemError::NotDirectory);
        }
        if parent.by_entry_index {
            // a directory with a corrupt first cluster
            return Err(FileSystemError::IoError);
        }

        let parent_cluster = parent.cluster_or_entry_index;
        let fat32_name = to_fat32_name(name)?;

        let entries = self.read_directory_entries(parent_cluster)?;
        if entries
            .iter()
            .any(|e| e.is_short_name_node() && e.full_name_matches(&fat32_name))
        {
            return Err(FileSystemError::FileExists);
        }

        let free_slot = self.find_free_slot_in_directory(parent_cluster, &entries)?;

        let mut new_entry = DirectoryEntry::create_empty();
        new_entry.set_filename(name)?;

        if is_new_node_directory {
            let new_dir_cluster = self.allocate_cluster(NO_CLUSTER)?;
            let dot_entry = DirectoryEntry::create_dot_entry(new_dir_cluster);
            // the parent of a top level directory is recorded as cluster 0 in ".."
            let dotdot_cluster = if parent_cluster == self.root_cluster {
                NO_CLUSTER
            } else {
                parent_cluster
            };
            let dotdot_entry = DirectoryEntry::create_dot_dot_entry(dotdot_cluster);

            // allocate_cluster left the buffer zeroed
            self.cluster_buffer[..DIRECTORY_ENTRY_SIZE].copy_from_slice(&dot_entry.as_bytes());
            self.cluster_buffer[DIRECTORY_ENTRY_SIZE..2 * DIRECTORY_ENTRY_SIZE]
                .copy_from_slice(&dotdot_entry.as_bytes());
            self.write_cluster_from_buffer(new_dir_cluster)?;

            new_entry.attributes = FatFileAttributes::Directory as u8;
            new_entry.set_first_cluster(new_dir_cluster);
        } else {
            // the first cluster is allocated by the first write
            new_entry.attributes = FatFileAttributes::Archive as u8;
        }
        new_entry.file_size = 0;

        self.write_direntry(parent_cluster, free_slot, &new_entry)?;

        Ok(encode_node_id(&new_entry, parent_cluster, free_slot))
    }

    pub fn create_file(
        &mut self,
        parent_id: FileNodeHandle,
        name: &str,
    ) -> FileSystemResult<FileNodeHandle> {
        self.create_node(parent_id, name, false)
    }

    pub fn create_directory(
        &mut self,
        parent_id: FileNodeHandle,
        name: &str,
    ) -> FileSystemResult<FileNodeHandle> {
        self.create_node(parent_id, name, true)
    }

    pub fn delete(&mut self, node_id: FileNodeHandle) -> FileSystemResult<()> {
        if self.is_root_node(node_id) {
            return Err(FileSystemError::PermissionDenied);
        }

        let parent_cluster = decode_node_id(node_id).parent_cluster;
        let (mut entry, entry_index) = self.load_entry(node_id)?;
        let first_cluster = entry.get_first_cluster();

        if entry.is_directory() {
            let dir_entries = self.read_directory_entries(first_cluster)?;
            if dir_entries.iter().any(|e| {
                e.is_short_name_node() && !e.is_current_directory() && !e.is_parent_directory()
            }) {
                return Err(FileSystemError::DirectoryNotEmpty);
            }
        }

        // the entry is marked deleted first so a failure never leaves it pointing at freed clusters
        entry.mark_deleted();
        self.write_direntry(parent_cluster, entry_index, &entry)?;
        self.free_cluster_chain(first_cluster)
    }
}
