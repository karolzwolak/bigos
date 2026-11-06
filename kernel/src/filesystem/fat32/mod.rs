pub mod boot_sector;
pub mod direntry;
pub mod test_data;

use crate::filesystem::fat32::direntry::{FatFileAttributes, MAX_EXT_LENGTH, MAX_NAME_LENGTH};
use crate::filesystem::sirius::{
    FileAttributes, FileNode, FileSystemError, FileSystemResult, FileType, FilesystemDriver,
};
use crate::io::disk::{DiskManager, get_disk_mgr};
use crate::serial_println;
use alloc::string::String;
use alloc::vec::Vec;
use boot_sector::BootSector;
use direntry::{DIRECTORY_ENTRY_SIZE, DirectoryEntry, MAX_FULL_NAME_LENGTH};
use spin::MutexGuard;

pub const ROOT_CLUSTER: u32 = 2;
// node_id packing assumes up to 2^24 clusters
pub const MAX_CLUSTER: u32 = 0xFFFFFF;
const EMPTY_FAT_ENTRY: u32 = 0;

pub type FileNodeHandle = usize;
pub const INVALID_NODE_HANDLE: FileNodeHandle = 0;

const fn is_directory(fat_attributes: u8) -> bool {
    fat_attributes & FatFileAttributes::Directory as u8 != 0
}

fn is_valid_filename(name: &str) -> bool {
    if name.is_empty() || name.starts_with('.') {
        return false;
    }
    match name.rfind('.') {
        Some(dot) => dot <= MAX_NAME_LENGTH && (name.len() - dot - 1) <= MAX_EXT_LENGTH,
        None => name.len() <= MAX_NAME_LENGTH,
    }
}

const ZERO_BUFFER: [u8; 4096] = [0u8; 4096]; // TODO: dont allocate each call, but also dont fill space with this --> write in some other way than copying from buffer

fn encode_node_id(entry: &DirectoryEntry, parent_cluster: u32) -> FileNodeHandle {
    let cluster = entry.get_first_cluster();
    let reserved_flag = 0; // can use for something
    let attrs = entry.attributes;

    ((reserved_flag as usize) << 63)
        | ((attrs as usize & 0x7F) << 56)
        | ((parent_cluster as usize & 0xFFFFFF) << 32)
        | (cluster as usize & 0xFFFFFF)
}

fn decode_node_id(node_id: FileNodeHandle) -> (u32, u32, u8) {
    let attrs = ((node_id >> 56) & 0x7F) as u8;
    let parent_cluster = ((node_id >> 32) & 0xFFFFFF) as u32;
    let cluster = (node_id & 0xFFFFFF) as u32;

    (cluster, parent_cluster, attrs)
}

fn to_fat32_name(name: &str) -> FileSystemResult<[u8; 11]> {
    if !is_valid_filename(name) {
        return Err(FileSystemError::InvalidFilename);
    }

    let mut fat32_name = [b' '; 11];

    let (stem, ext) = if let Some(dot_pos) = name.rfind('.') {
        (&name[..dot_pos], Some(&name[dot_pos + 1..]))
    } else {
        (name, None)
    };

    if stem.len() > MAX_NAME_LENGTH {
        return Err(FileSystemError::InvalidFilename);
    }
    if let Some(ext) = ext
        && ext.len() > MAX_EXT_LENGTH
    {
        return Err(FileSystemError::InvalidFilename);
    }

    for (i, c) in stem.chars().enumerate() {
        fat32_name[i] = c.to_ascii_uppercase() as u8;
    }
    if let Some(ext) = ext {
        for (i, c) in ext.chars().enumerate() {
            fat32_name[MAX_NAME_LENGTH + i] = c.to_ascii_uppercase() as u8;
        }
    }

    Ok(fat32_name)
}

fn _to_fat32_path(path: &str) -> FileSystemResult<Vec<[u8; MAX_FULL_NAME_LENGTH]>> {
    let path_parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();

    let mut fat32_parts = Vec::<[u8; MAX_FULL_NAME_LENGTH]>::with_capacity(path_parts.len());

    for part in path_parts {
        if part == "." || part == ".." {
            //TODO: handle user wanting to do "../some_file_in_parent_dir_relative" etc, when we get process working directories, but this is probably not the place to resolve these
            return Err(FileSystemError::InvalidPath);
        } else {
            fat32_parts.push(to_fat32_name(part)?);
        }
    }

    Ok(fat32_parts)
}

const fn file_attributes_from_fat_attributes(fat_attributes: u8) -> FileAttributes {
    if is_directory(fat_attributes) {
        FileAttributes::DIR_DEFAULT
    } else {
        if fat_attributes & FatFileAttributes::ReadOnly as u8 == 0 {
            FileAttributes::FILE_READ_WRITE
        } else {
            FileAttributes::FILE_READONLY
        }
    }
}

pub struct Fat32Driver {
    boot_sector: BootSector,
    fat_start_sector: u64,
    root_start_sector: u64,
    data_start_sector: u64,
    sectors_per_cluster: u32,

    cluster_size: usize,

    max_cluster: u32,

    root_dir_node_id: FileNodeHandle,
    root_direntry: DirectoryEntry,
    root_filenode: FileNode,
}

pub const END_OF_CHAIN: u32 = 0x0FFFFFFF;
pub const BAD_CLUSTER: u32 = 0xFFFFFFF7;
pub const FAT_ENTRY_RESERVED_BEGIN: u32 = 0xFFFFFFF8;
pub const FAT_ENTRY_RESERVED_END: u32 = 0xFFFFFFFE;

impl Fat32Driver {
    pub fn new(boot_sector_data: &[u8]) -> FileSystemResult<Self> {
        serial_println!("Fat32Driver: initializing with boot_sector_data");

        let boot_sector = BootSector::from_bytes(boot_sector_data)?;

        let fat_start_sector = boot_sector.reserved_sectors as u64;
        let sectors_per_cluster = boot_sector.sectors_per_cluster as u32;
        let fat_size = boot_sector.fat_size_32 as u64;
        let root_start_sector = fat_start_sector + (boot_sector.num_fats as u64 * fat_size);
        let root_dir_sectors = 0u64;
        let data_start_sector = root_start_sector + root_dir_sectors;
        let cluster_size = (sectors_per_cluster as usize) * (boot_sector.bytes_per_sector as usize);

        let root_direntry = DirectoryEntry::create_for_root();
        let root_dir_node_id = encode_node_id(&root_direntry, boot_sector.root_cluster);
        let root_filenode = FileNode {
            node_id: root_dir_node_id,
            name: String::from("/"),
            file_type: FileType::Directory,
            size: 0,
            created_time: 0,
            modified_time: 0,
            attributes: FileAttributes::DIR_DEFAULT,
        };

        let available_data_sectors = boot_sector.total_sectors_32 - data_start_sector as u32;
        let total_clusters = available_data_sectors / sectors_per_cluster;
        let max_cluster = total_clusters + ROOT_CLUSTER;

        serial_println!("Initialized Fat32Driver with boot sector data: \nfat_start_sector={},\n sectors_per_cluster={},\n 
            fat_size={},\n root_start_sector={},\n root_dir_sectors={},\n data_start_sector={},\n cluster_size={},\n max_cluster={}", fat_start_sector, sectors_per_cluster, 
            fat_size, root_start_sector, root_dir_sectors, data_start_sector, cluster_size, max_cluster);

        Ok(Self {
            boot_sector,
            fat_start_sector,
            root_start_sector,
            data_start_sector,
            sectors_per_cluster,
            cluster_size,
            max_cluster,
            root_dir_node_id,
            root_direntry,
            root_filenode,
        })
    }

    fn cluster_to_sector(&self, cluster: u32) -> u64 {
        if cluster >= 2 {
            self.data_start_sector + (((cluster - 2) as u64) * self.sectors_per_cluster as u64)
        } else {
            self.root_start_sector
        }
    }

    fn read_fat_entry(
        &self,
        cluster: u32,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<u32> {
        let fat_offset = (cluster as u64) * 4;
        let fat_sector =
            self.fat_start_sector + (fat_offset / self.boot_sector.bytes_per_sector as u64);
        let offset_in_sector = (fat_offset % self.boot_sector.bytes_per_sector as u64) as usize;

        let mut sector_buffer = alloc::vec![0u8; self.boot_sector.bytes_per_sector as usize];

        {
            //let mut disk_mgr = get_disk_mgr();
            disk_mgr.read_sector(fat_sector, &mut sector_buffer)?
        }

        let entry_bytes = [
            sector_buffer[offset_in_sector],
            sector_buffer[offset_in_sector + 1],
            sector_buffer[offset_in_sector + 2],
            sector_buffer[offset_in_sector + 3],
        ];

        Ok(u32::from_le_bytes(entry_bytes))
    }

    fn get_next_cluster(
        &self,
        cluster: u32,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<Option<u32>> {
        if cluster == 0 {
            return Ok(None);
        }

        let fat_entry = self.read_fat_entry(cluster, disk_mgr)?;
        serial_println!("get_next_cluster returning {}", fat_entry);

        match fat_entry {
            END_OF_CHAIN => Ok(None),
            BAD_CLUSTER => Err(FileSystemError::DiskOpError),
            FAT_ENTRY_RESERVED_BEGIN..=FAT_ENTRY_RESERVED_END => Ok(None),
            next_cluster => Ok(Some(next_cluster)),
        }
    }

    fn read_cluster_chain(
        &self,
        start_cluster: u32,
        out_buffer: &mut [u8],
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<usize> {
        let mut curr_cluster = start_cluster;
        let mut bytes_read = 0;

        while bytes_read < out_buffer.len() && curr_cluster < self.max_cluster {
            let sector = self.cluster_to_sector(curr_cluster);
            let bytes_to_read = core::cmp::min(self.cluster_size, out_buffer.len() - bytes_read);

            {
                let mut cluster_buffer = alloc::vec![0u8; self.cluster_size];
                //let mut disk_mgr = get_disk_mgr();
                disk_mgr.read_sectors(
                    sector,
                    self.sectors_per_cluster as usize,
                    &mut cluster_buffer,
                )?;
                out_buffer[bytes_read..bytes_read + bytes_to_read]
                    .copy_from_slice(&cluster_buffer[..bytes_to_read]);

                bytes_read += bytes_to_read;

                match self.get_next_cluster(curr_cluster, disk_mgr)? {
                    Some(next) => curr_cluster = next,
                    None => break,
                }
            }
        }

        Ok(bytes_read)
    }

    fn get_cluster_chain_length(
        &self,
        start_cluster: u32,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<usize> {
        let mut length = 0;
        let mut curr_cluster = start_cluster;

        while curr_cluster >= ROOT_CLUSTER && curr_cluster < self.max_cluster {
            length += 1;
            match self.get_next_cluster(curr_cluster, disk_mgr)? {
                Some(next) => curr_cluster = next,
                None => break,
            }
        }

        Ok(length)
    }

    fn find_free_cluster(
        &self,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<u32> {
        for cluster in ROOT_CLUSTER..self.max_cluster {
            let fat_entry = self.read_fat_entry(cluster, disk_mgr)?;
            serial_println!(
                "find_free_cluster: read next fat_entry ({}) for cluster {}",
                fat_entry,
                cluster
            );

            if fat_entry == 0 {
                serial_println!("  Found free cluster: {}", cluster);
                return Ok(cluster);
            }
        }

        serial_println!("ERROR: find_free_cluster: could not find a free cluster");

        Err(FileSystemError::NoSpace)
    }

    fn allocate_clusters(
        &mut self,
        count: usize,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<u32> {
        assert!(count != 0);

        let first_cluster = self.find_free_cluster(disk_mgr)?;
        serial_println!("  Allocating cluster chain starting at: {}", first_cluster);

        let mut prev_cluster = first_cluster;
        for _ in 1..count {
            let new_cluster = self.find_free_cluster(disk_mgr)?;
            serial_println!("    Allocating new cluster: {}", new_cluster);

            // link the previous cluster
            self.write_fat_entry(prev_cluster, new_cluster, disk_mgr)?;

            prev_cluster = new_cluster;
        }
        self.write_fat_entry(prev_cluster, END_OF_CHAIN, disk_mgr)?;

        Ok(first_cluster)
    }

    fn clear_clusters(
        &mut self,
        start_cluster: u32,
        count: u32,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<()> {
        let mut curr_cluster = start_cluster;
        serial_println!(
            "clear_clusters: starting with start_cluster: {}, to clear count: {}",
            start_cluster,
            count
        );

        {
            //let mut disk_mgr = get_disk_mgr();

            for _ in 0..count {
                let sector = self.cluster_to_sector(curr_cluster);
                serial_println!(
                    "clear_clusters: clearing cluster {}, sector {}",
                    curr_cluster,
                    sector
                );

                for i in 0..self.sectors_per_cluster {
                    serial_println!("clear_clusters: writing sector {}", i);
                    disk_mgr.write_sector(sector + i as u64, &ZERO_BUFFER)?;
                }
                serial_println!("clear_clusters: finished writing");

                match self.get_next_cluster(curr_cluster, disk_mgr)? {
                    Some(next) => {
                        serial_println!("clear_clusters: moving to next cluster: {}", next);
                        curr_cluster = next
                    }
                    None => break,
                }
            }
        }

        serial_println!("clear_clusters: finished");

        Ok(())
    }

    fn write_fat_entry(
        &mut self,
        cluster: u32,
        value: u32,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<()> {
        let fat_offset = (cluster as u64) * 4;
        let fat_sector =
            self.fat_start_sector + (fat_offset / self.boot_sector.bytes_per_sector as u64);
        let offset_in_sector = (fat_offset % self.boot_sector.bytes_per_sector as u64) as usize;

        let value_bytes = value.to_le_bytes();

        let mut sector_buffer = alloc::vec![0u8; self.boot_sector.bytes_per_sector as usize];

        {
            //TODO: buffer this write, write the whole ready sector buffer after all operations
            //let mut disk_mgr = get_disk_mgr();
            disk_mgr.read_sector(fat_sector, &mut sector_buffer)?;

            sector_buffer[offset_in_sector..offset_in_sector + 4].copy_from_slice(&value_bytes);
            disk_mgr.write_sector(fat_sector, &sector_buffer)?;
        }

        Ok(())
    }

    // Read directory entries starting with a given cluster
    // NOTE: this returns all entries, including deleted ones, volume id and . / .. entries,
    // since we use index of the entry in this returned vector for some operations
    fn read_directory_entries(
        &self,
        start_cluster: u32,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<Vec<DirectoryEntry>> {
        let cluster_count = self.get_cluster_chain_length(start_cluster, disk_mgr)?;
        let buffer_size = cluster_count * self.cluster_size;
        let mut buffer = alloc::vec![0u8; buffer_size];

        let bytes_read = self.read_cluster_chain(start_cluster, &mut buffer, disk_mgr)?;

        let entry_count = bytes_read / DIRECTORY_ENTRY_SIZE;
        let mut valid_entries = Vec::with_capacity(entry_count);

        serial_println!(
            "read_directory_entries: cluster_chain_length: {}, bytes_read: {}, entry_count: {}",
            cluster_count,
            bytes_read,
            entry_count
        );

        for i in 0..entry_count {
            let offset = i * DIRECTORY_ENTRY_SIZE;
            let entry = DirectoryEntry::from_bytes(&buffer[offset..offset + DIRECTORY_ENTRY_SIZE]);

            if entry.is_empty() {
                serial_println!(
                    "read_directory_entries: Hit end of directory at entry {}",
                    i
                );
                break;
            }

            serial_println!(
                "  Entry {}: name={}, attr={:#x}, deleted={}",
                i,
                entry.get_filename(),
                entry.attributes,
                entry.is_deleted()
            );

            valid_entries.push(entry);
        }

        Ok(valid_entries)
    }

    fn find_entry_by_cluster(
        &self,
        parent_cluster: u32,
        target_cluster: u32,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<DirectoryEntry> {
        let entries = self.read_directory_entries(parent_cluster, disk_mgr)?;

        for entry in entries {
            if entry.get_first_cluster() == target_cluster {
                return Ok(entry);
            }
        }

        Err(FileSystemError::NotFound)
    }

    fn find_direntry(
        &self,
        path: &str,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<(DirectoryEntry, u32)> {
        let path_parts: Vec<&str> = path.split("/").filter(|p| !p.is_empty()).collect();
        let part_count = path_parts.len();

        if path_parts.is_empty() {
            return Ok((self.root_direntry, self.boot_sector.root_cluster));
        }

        let mut found_entry = None;
        let mut curr_cluster = self.boot_sector.root_cluster;
        let mut parent_cluster = self.boot_sector.root_cluster;

        for (i, path_part) in path_parts.iter().enumerate() {
            let fat32_path_part = to_fat32_name(path_part)?;
            serial_println!(
                "FAT32Driver: find_direntry: path part {} - {} = {:?}",
                i,
                *path_part,
                fat32_path_part
            );

            let entries = self.read_directory_entries(curr_cluster, disk_mgr)?;
            for e in &entries {
                serial_println!(
                    "   FAT32Driver: find_direntry: entry {}, is equal?: {}",
                    e.get_filename(),
                    e.full_name_matches(&fat32_path_part)
                );
            }

            let entry = entries
                .iter()
                .find(|e| !e.is_deleted() && e.full_name_matches(&fat32_path_part))
                .ok_or(FileSystemError::NotFound)?;

            if i < part_count - 1 && !entry.is_directory() {
                return Err(FileSystemError::NotDirectory);
            }

            parent_cluster = curr_cluster;
            curr_cluster = entry.get_first_cluster();
            found_entry = Some(*entry);
        }

        assert!(found_entry.is_some());
        let entry = found_entry.ok_or(FileSystemError::NotFound)?;

        Ok((entry, parent_cluster))
    }

    fn write_direntry(
        &mut self,
        dir_cluster: u32,
        index_in_directory: usize,
        entry: &DirectoryEntry,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<()> {
        //TODO: try to avoid reading cluster chain into buffer here - pass an already filled buffer (would have to assure thread safety with a big disk maanager guard or have some sort of a cluster-level guard?)
        let cluster_count = self.get_cluster_chain_length(dir_cluster, disk_mgr)?;
        let buffer_size = cluster_count * self.cluster_size;
        let mut dir_buffer = alloc::vec![0u8; buffer_size];

        let bytes_read = self.read_cluster_chain(dir_cluster, &mut dir_buffer, disk_mgr)?;

        let entry_offset = index_in_directory * DIRECTORY_ENTRY_SIZE;
        let entry_end = entry_offset + DIRECTORY_ENTRY_SIZE;
        if entry_end > bytes_read {
            return Err(FileSystemError::NoSpace);
        }

        let entry_bytes = entry.as_bytes();
        dir_buffer[entry_offset..entry_end].copy_from_slice(&entry_bytes);

        // write the buffer to disk
        let mut bytes_written = 0;
        let buffer_size = dir_buffer.len();
        let sectors_per_cluster = self.sectors_per_cluster as usize;
        let mut curr_cluster = dir_cluster;

        {
            //let mut disk_mgr = get_disk_mgr();

            while bytes_written < buffer_size && curr_cluster < self.max_cluster {
                let sector = self.cluster_to_sector(curr_cluster);
                let chunk_size = core::cmp::min(self.cluster_size, buffer_size - bytes_written);

                disk_mgr.write_sectors(
                    sector,
                    sectors_per_cluster,
                    &dir_buffer[bytes_written..bytes_written + chunk_size],
                )?;

                bytes_written += chunk_size;

                match self.get_next_cluster(curr_cluster, disk_mgr)? {
                    Some(next) => curr_cluster = next,
                    None => break,
                }
            }
        }

        Ok(())
    }

    fn expand_directory(
        &mut self,
        dir_cluster: u32,
        curr_entry_count: usize,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<usize> {
        let mut last_cluster = dir_cluster;
        while last_cluster >= ROOT_CLUSTER && last_cluster < self.max_cluster {
            match self.get_next_cluster(last_cluster, disk_mgr)? {
                Some(next) => last_cluster = next,
                None => break,
            }
        }
        serial_println!("expand_directory: last_cluster = {}", last_cluster);

        let new_cluster = self.allocate_clusters(1, disk_mgr)?;
        serial_println!("expand_directory: allocated new_cluster = {}", new_cluster);
        self.clear_clusters(new_cluster, 1, disk_mgr)?;
        serial_println!("expand_directory: cleared new cluster");

        self.write_fat_entry(last_cluster, new_cluster, disk_mgr)?;
        self.write_fat_entry(new_cluster, END_OF_CHAIN, disk_mgr)?;
        let new_slot_index = curr_entry_count;
        serial_println!(
            "expand_directory: linked clusters {} and {}, returning new slot index: {}",
            last_cluster,
            new_cluster,
            new_slot_index
        );

        Ok(new_slot_index)
    }

    fn free_cluster_chain(
        &mut self,
        start_cluster: u32,
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<()> {
        let mut curr_cluster = start_cluster;

        while curr_cluster >= ROOT_CLUSTER && curr_cluster < self.max_cluster {
            let next = match self.get_next_cluster(curr_cluster, disk_mgr)? {
                Some(next_cluster) => {
                    assert!(next_cluster >= ROOT_CLUSTER && next_cluster < self.max_cluster);
                    Some(next_cluster)
                }
                None => None,
            };

            // free the current cluster
            self.write_fat_entry(curr_cluster, EMPTY_FAT_ENTRY, disk_mgr)?;
            serial_println!("free_cluster_chain: freed cluster {}", curr_cluster);

            match next {
                Some(next_cluster) => curr_cluster = next_cluster,
                None => break,
            }
        }

        Ok(())
    }

    fn find_free_slot_in_directory(
        &mut self,
        dir_cluster: u32,
        entries: &[DirectoryEntry],
        disk_mgr: &mut MutexGuard<'_, DiskManager>,
    ) -> FileSystemResult<usize> {
        serial_println!(
            "FAT32Driver: find_free_slot_in_directory(): reading directory entries from a given entry list"
        );
        for (i, entry) in entries.iter().enumerate() {
            if !entry.is_valid() {
                serial_println!(
                    "FAT32Driver: find_free_slot_in_directory(): found free entry at slot: {}",
                    i
                );
                return Ok(i);
            }
        }

        serial_println!(
            "FAT32Driver: find_free_slot_in_directory(): no free entry found, expanding directory"
        );
        let new_slot = self.expand_directory(dir_cluster, entries.len(), disk_mgr)?;
        serial_println!(
            "FAT32Driver: find_free_slot_in_directory(): expanded directory: new_slot: {}",
            new_slot
        );

        Ok(new_slot)
    }
}

impl FilesystemDriver for Fat32Driver {
    fn read_file(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        out_buffer: &mut [u8],
    ) -> FileSystemResult<usize> {
        let (cluster, parent_cluster, attributes) = decode_node_id(node_id);

        serial_println!(
            "FAT32Driver: read_file called with node_id={:#x}, cluster={}, offset={}, out_buffer_size={}",
            node_id,
            cluster,
            offset,
            out_buffer.len()
        );

        if is_directory(attributes) {
            return Err(FileSystemError::IsDirectory);
        }

        if cluster == 0 {
            return Err(FileSystemError::InvalidPath);
        }

        let mut disk_mgr = get_disk_mgr();

        let entry = self.find_entry_by_cluster(parent_cluster, cluster, &mut disk_mgr)?;
        let file_size = entry.file_size as usize;

        if offset >= file_size {
            serial_println!(
                "FAT32Driver: Offset {} is beyond file size {}",
                offset,
                file_size
            );
            return Err(FileSystemError::FileSizeExceeded);
        }

        let skip_clusters = offset / self.cluster_size;
        let offset_in_cluster = offset % self.cluster_size;

        // Skip to correct cluster
        serial_println!(
            "FAT32Driver: Reading file at cluster {}, offset_in_cluster {}, skip_clusters {}",
            cluster,
            offset_in_cluster,
            skip_clusters
        );

        let mut curr_cluster = cluster;

        for _ in 0..skip_clusters {
            match self.get_next_cluster(curr_cluster, &mut disk_mgr)? {
                Some(next) => curr_cluster = next,
                None => return Ok(0),
            }
        }

        serial_println!(
            "FAT32Driver: Positioned at cluster {} after skipping",
            curr_cluster
        );

        // Read cluster and offset
        let mut temp_buffer = alloc::vec![0u8; self.cluster_size];
        let sector = self.cluster_to_sector(curr_cluster);
        serial_println!(
            "FAT32Driver: Reading cluster {}, sector {}, offset_in_cluster {}",
            curr_cluster,
            sector,
            offset_in_cluster
        );

        {
            serial_println!(
                "FAT32Driver: Issuing read_sectors for sector {}, count {}, temp_buffer size {}",
                sector,
                self.sectors_per_cluster,
                temp_buffer.len()
            );
            //let mut disk_mgr = get_disk_mgr();
            disk_mgr.read_sectors(sector, self.sectors_per_cluster as usize, &mut temp_buffer)?
        }

        let available = self.cluster_size - offset_in_cluster;
        let bytes_to_read = core::cmp::min(
            available,
            core::cmp::min(out_buffer.len(), file_size - offset),
        );

        serial_println!(
            "FAT32Driver: Reading cluster {}, sector {}, offset_in_cluster {}, bytes_to_read {}",
            curr_cluster,
            sector,
            offset_in_cluster,
            bytes_to_read
        );

        out_buffer[..bytes_to_read]
            .copy_from_slice(&temp_buffer[offset_in_cluster..offset_in_cluster + bytes_to_read]);

        serial_println!(
            "FAT32Driver: Read {} bytes from file (offset {})",
            bytes_to_read,
            offset
        );

        Ok(bytes_to_read)
    }

    fn write_file(
        &mut self,
        _node_id: FileNodeHandle,
        _offset: usize,
        _data: &[u8],
    ) -> FileSystemResult<usize> {
        Err(FileSystemError::NotSupported)
    }

    fn get_node(&self, node_id: FileNodeHandle) -> FileSystemResult<FileNode> {
        let (cluster, parent_cluster, _) = decode_node_id(node_id);

        if cluster != ROOT_CLUSTER {
            let mut disk_mgr = get_disk_mgr();
            let entry = self.find_entry_by_cluster(parent_cluster, cluster, &mut disk_mgr)?;

            Ok(FileNode {
                node_id,
                name: entry.get_filename(),
                file_type: entry.get_file_type(),
                size: entry.file_size as usize,
                created_time: entry.get_creation_timestamp(),
                modified_time: entry.get_modified_timestamp(),
                attributes: file_attributes_from_fat_attributes(entry.attributes),
            })
        } else {
            Ok(self.root_filenode.clone())
        }
    }

    fn list_directory(&self, node_id: FileNodeHandle) -> FileSystemResult<Vec<FileNode>> {
        let (dir_cluster, _, attributes) = decode_node_id(node_id);

        if !is_directory(attributes) {
            return Err(FileSystemError::NotDirectory);
        }

        let mut entries;
        {
            let mut disk_mgr = get_disk_mgr();
            entries = self.read_directory_entries(dir_cluster, &mut disk_mgr)?;
            entries.retain(|e| e.is_valid() && !e.is_volume_id());
        }
        let mut nodes = Vec::with_capacity(entries.len());

        serial_println!(
            "FAT32Driver: list_directory found {} entries in dir_cluster {}",
            entries.len(),
            dir_cluster
        );

        for entry in entries {
            let entry_cluster = entry.get_first_cluster();
            let is_dir = entry.is_directory();
            let size = entry.file_size;
            let node_id = encode_node_id(&entry, dir_cluster);

            serial_println!(
                "FAT32Driver: list_directory Encoding file '{}' with cluster={}, size={}, is_dir={}, node_id={:#x}",
                entry.get_filename(),
                entry_cluster,
                size,
                is_dir,
                node_id
            );

            nodes.push(FileNode {
                node_id,
                name: entry.get_filename(),
                file_type: entry.get_file_type(),
                size: size as usize,
                created_time: entry.get_creation_timestamp(),
                modified_time: entry.get_modified_timestamp(),
                attributes: file_attributes_from_fat_attributes(entry.attributes),
            });
        }

        Ok(nodes)
    }

    fn find_node(&self, path: &str) -> FileSystemResult<FileNodeHandle> {
        let mut disk_mgr = get_disk_mgr();
        let (entry, parent_cluster) = self.find_direntry(path, &mut disk_mgr)?;

        let is_dir = entry.is_directory();
        let size = entry.file_size;

        serial_println!(
            "FAT32Driver: find_node found '{}' at cluster {}, parent_cluster {}, is_dir={}",
            path,
            entry.get_first_cluster(),
            parent_cluster,
            is_dir
        );
        assert!(!(is_dir && size != 0));

        let node_id = encode_node_id(&entry, parent_cluster);
        serial_println!("find_node: Encoding node_id {:#x}", node_id);

        Ok(node_id)
    }

    fn create_file(
        &mut self,
        parent_id: FileNodeHandle,
        name: &str,
    ) -> FileSystemResult<FileNodeHandle> {
        //TODO: there is possible thread overriding each other if i dont put a guard around the whole thing,
        // including reading clusters and until the last write - maybe do a cluster-level guard (some busy flag?) (locking the
        // whole disk is not a great idea)
        let (parent_cluster, _, attrs) = decode_node_id(parent_id);
        serial_println!(
            "FAT32Driver: create_file: called for parent with node id: {:#x}",
            parent_id
        );

        if !is_directory(attrs) {
            serial_println!("FAT32Driver: create_file: parent is not a directory");
            return Err(FileSystemError::NotDirectory);
        }

        if !is_valid_filename(name) {
            serial_println!("FAT32Driver: create_file: filename is invalid");
            return Err(FileSystemError::InvalidFilename);
        }

        let mut entry = DirectoryEntry::create_empty();

        {
            let mut disk_mgr = get_disk_mgr();

            let entries = self.read_directory_entries(parent_cluster, &mut disk_mgr)?;
            if entries
                .iter()
                .any(|e| !e.is_deleted() && e.get_filename() == name)
            {
                serial_println!("FAT32Driver: create_file: entry already exists");
                return Err(FileSystemError::FileExists);
            }

            let free_slot_index =
                self.find_free_slot_in_directory(parent_cluster, &entries, &mut disk_mgr)?;
            serial_println!(
                "FAT32Driver: create_file: free_slot_index: {}",
                free_slot_index
            );

            let new_file_cluster = self.allocate_clusters(1, &mut disk_mgr)?;
            serial_println!(
                "FAT32Driver: create_file: new_file_cluster: {}",
                new_file_cluster
            );
            self.clear_clusters(new_file_cluster, 1, &mut disk_mgr)?;
            serial_println!("FAT32Driver: create_file: New file cluster cleared");

            let _timestamp = 0; //TODO: pass to constructor
            entry.set_filename(name);
            entry.attributes = FatFileAttributes::Archive as u8;
            entry.set_first_cluster(new_file_cluster);

            self.write_direntry(parent_cluster, free_slot_index, &entry, &mut disk_mgr)?;
            serial_println!("FAT32Driver: create_file: new directory entry written");
        }

        let node_id = encode_node_id(&entry, parent_cluster);

        Ok(node_id)
    }

    fn create_directory(
        &mut self,
        parent_id: FileNodeHandle,
        name: &str,
    ) -> FileSystemResult<FileNodeHandle> {
        let (parent_cluster, _, attrs) = decode_node_id(parent_id);
        serial_println!(
            "FAT32Driver: create_directory: called for parent with node id: {:#x}",
            parent_id
        );

        if !is_directory(attrs) {
            serial_println!("FAT32Driver: create_directory: parent is not a directory");
            return Err(FileSystemError::NotDirectory);
        }

        if !is_valid_filename(name) {
            serial_println!("FAT32Driver: create_directory: name is invalid");
            return Err(FileSystemError::InvalidFilename);
        }

        let mut new_dir_entry = DirectoryEntry::create_empty();

        {
            let mut disk_mgr = get_disk_mgr();

            let entries = self.read_directory_entries(parent_cluster, &mut disk_mgr)?;
            if entries
                .iter()
                .any(|e| !e.is_deleted() && e.get_filename() == name)
            {
                serial_println!("FAT32Driver: create_directory: entry already exists");
                return Err(FileSystemError::FileExists);
            }
            let free_slot_index =
                self.find_free_slot_in_directory(parent_cluster, &entries, &mut disk_mgr)?;
            serial_println!(
                "FAT32Driver: create_directory: free_slot_index: {}",
                free_slot_index
            );

            let new_dir_cluster = self.allocate_clusters(1, &mut disk_mgr)?;
            serial_println!(
                "FAT32Driver: create_directory: allocated cluster: {}",
                new_dir_cluster
            );

            self.clear_clusters(new_dir_cluster, 1, &mut disk_mgr)?;
            serial_println!("FAT32Driver: create_directory: cleared new cluster");

            let dot_entry = DirectoryEntry::create_dot_entry(new_dir_cluster);
            let dotdot_entry = DirectoryEntry::create_dot_dot_entry(parent_cluster);

            self.write_direntry(new_dir_cluster, 0, &dot_entry, &mut disk_mgr)?;
            self.write_direntry(new_dir_cluster, 1, &dotdot_entry, &mut disk_mgr)?;
            serial_println!("FAT32Driver: create_directory: wrote . and .. entries");

            new_dir_entry.set_filename(name);
            new_dir_entry.attributes = FatFileAttributes::Directory as u8;
            new_dir_entry.set_first_cluster(new_dir_cluster);
            new_dir_entry.file_size = 0;

            //TODO: timestamp

            self.write_direntry(
                parent_cluster,
                free_slot_index,
                &new_dir_entry,
                &mut disk_mgr,
            )?;
            serial_println!("FAT32Driver: create_directory: wrote new directory entry to parent");
        }

        let node_id = encode_node_id(&new_dir_entry, parent_cluster);
        serial_println!(
            "FAT32Driver: create_directory: created directory with node_id: {:#x}",
            node_id
        );

        Ok(node_id)
    }

    fn delete(&mut self, node_id: FileNodeHandle) -> FileSystemResult<()> {
        let (cluster, parent_cluster, attrs) = decode_node_id(node_id);
        serial_println!(
            "FAT32Driver: delete: called for node_id: {:#x}, cluster: {}, parent_cluster: {}",
            node_id,
            cluster,
            parent_cluster
        );

        {
            let mut disk_mgr = get_disk_mgr();

            let entries = self.read_directory_entries(parent_cluster, &mut disk_mgr)?;
            let (entry_index, entry) = entries
                .iter()
                .enumerate()
                .find(|(_, e)| !e.is_deleted() && e.get_first_cluster() == cluster)
                .ok_or(FileSystemError::NotFound)?;

            serial_println!("FAT32Driver: delete: found entry at index {}", entry_index);

            if is_directory(attrs) {
                let dir_entries = self.read_directory_entries(cluster, &mut disk_mgr)?;
                let has_valid_entries = dir_entries
                    .iter()
                    .any(|e| e.is_valid() && e.name[0] != b'.');

                if has_valid_entries {
                    //TODO: a way to delete non-empty directories recursively
                    serial_println!("FAT32Driver: delete: directory is not empty");
                    return Err(FileSystemError::DirectoryNotEmpty);
                }

                self.free_cluster_chain(cluster, &mut disk_mgr)?;
                serial_println!("FAT32Driver: delete: freed directory clusters");
            } else {
                self.free_cluster_chain(cluster, &mut disk_mgr)?;
                serial_println!("FAT32Driver: delete: freed file clusters");
            }

            let mut deleted_entry = *entry;
            deleted_entry.mark_deleted();
            self.write_direntry(parent_cluster, entry_index, &deleted_entry, &mut disk_mgr)?;
            serial_println!("FAT32Driver: delete: marked entry as deleted");
        }

        Ok(())
    }

    fn root_node(&self) -> FileNodeHandle {
        self.root_dir_node_id
    }
}
