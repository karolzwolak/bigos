use crate::data_structures::hash_map_fx::FxHashMap;
use crate::filesystem::fat32::Fat32Driver;
use crate::filesystem::fat32::FileNodeHandle;
use crate::filesystem::file_cache::FS_CACHE_MAP_FILE_COUNT;
use crate::io::ata::AtaPioDriver;
use crate::io::disk::{DiskOpError, MockDiskDevice, SECTOR_SIZE, get_disk_mgr, init_disk};
use crate::serial_println_core;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use bitflags::bitflags;
use lazy_static::lazy_static;
use spin::{Mutex, Once};

#[cfg(feature = "use_cached_fs")]
use crate::filesystem::file_cache::{
    CacheFilesystemDriver, CacheImportance, CacheStats, FS_CACHE_SIZE, FileCache,
};

const DEBUG_LOGS: bool = false;

macro_rules! serial_println_core {
    ($($arg:tt)*) => {
        if DEBUG_LOGS {
            $crate::serial_println_core!($($arg)*);
        }
    };
}

#[cfg(feature = "use_cached_fs")]
lazy_static! {
    pub static ref SIRIUS: Once<Mutex<Sirius<CachedDriver<Fat32Driver>>>> = Once::new();
}

#[cfg(not(feature = "use_cached_fs"))]
lazy_static! {
    pub static ref SIRIUS: Once<Mutex<Sirius<Fat32Driver>>> = Once::new();
}

#[cfg(feature = "use_cached_fs")]
pub fn get_sirius() -> spin::MutexGuard<'static, Sirius<CachedDriver<Fat32Driver>>> {
    SIRIUS.get().unwrap().lock()
}

#[cfg(not(feature = "use_cached_fs"))]
pub fn get_sirius() -> spin::MutexGuard<'static, Sirius<Fat32Driver>> {
    SIRIUS.get().unwrap().lock()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FileType {
    File,
    Directory,
}

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct FileAttributes: u8 {
        const READ = 0b00000001;
        const WRITE = 0b00000010;
        const EXECUTE = 0b00000100;
        const HIDDEN = 0b00001000;
        // NOTE: careful with bit 7, node_id packing uses bits 56-62 for FAT attrs
        const RESERVED = 0b11110000;
    }
}

impl FileAttributes {
    pub const FILE_READONLY: Self = Self::READ;
    pub const FILE_READ_WRITE: Self = Self::READ.union(Self::WRITE);
    pub const FILE_READ_WRITE_EXECUTE: Self = Self::READ.union(Self::WRITE).union(Self::EXECUTE);
    pub const DIR_DEFAULT: Self = Self::READ;
}

// Canonical name length for DirEntryFlat. Must match the rustspace definition.
pub const FS_NAME_LEN: usize = 16;

/// Flat directory entry written into the userspace output buffer by sys_list_dir.
#[repr(C)]
pub struct DirEntryFlat {
    pub name: [u8; FS_NAME_LEN],
    pub name_len: u8,
    pub is_dir: u8,
    pub _pad: [u8; 6],
    pub size: u64,
    pub created_time: u32,
    pub modified_time: u32,
}

/// Flat stat result written into a userspace-provided buffer by sys_stat_file.
#[repr(C)]
pub struct StatFlat {
    pub name: [u8; FS_NAME_LEN],
    pub name_len: u8,
    pub is_dir: u8,
    pub size: u64,
    pub created_time: u32,
    pub modified_time: u32,
}

#[derive(Debug, Clone)]
pub struct FileNode {
    pub node_id: FileNodeHandle,
    pub name: String,
    pub file_type: FileType,
    pub size: usize,
    pub created_time: u32,
    pub modified_time: u32,
    pub attributes: FileAttributes,
}

pub type FileSystemResult<T> = Result<T, FileSystemError>;

#[derive(Debug, Clone, Copy)]
pub enum FileSystemError {
    NotFound,
    PermissionDenied,
    FileExists,
    IsDirectory,
    NotDirectory,
    DiskOpError,
    InvalidPath,
    FileSizeExceeded,
    InvalidFilename,
    DirectoryNotEmpty,
    NoSpace,
    DirectoryFull,
    IoError,
    FileNotFound,
    CacheFull,
    NotSupported,
}

impl From<DiskOpError> for FileSystemError {
    fn from(_: DiskOpError) -> Self {
        FileSystemError::DiskOpError
    }
}

pub trait FilesystemDriver: Send + Sync {
    fn read_file(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        out_buffer: &mut [u8],
    ) -> FileSystemResult<usize>;

    fn write_file(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        data: &[u8],
    ) -> FileSystemResult<usize>;

    fn find_node(&self, path: &str) -> FileSystemResult<FileNodeHandle>;
    fn get_node(&mut self, node_id: FileNodeHandle) -> FileSystemResult<FileNode>;
    fn list_directory(&mut self, node_id: FileNodeHandle) -> FileSystemResult<Vec<FileNode>>;

    fn create_file(
        &mut self,
        parent_id: FileNodeHandle,
        name: &str,
    ) -> FileSystemResult<FileNodeHandle>;
    fn create_directory(
        &mut self,
        parent_id: FileNodeHandle,
        name: &str,
    ) -> FileSystemResult<FileNodeHandle>;

    fn delete(&mut self, node_id: FileNodeHandle) -> FileSystemResult<()>;
    fn root_node(&self) -> FileNodeHandle;
}

#[cfg(feature = "use_cached_fs")]
pub struct FsDriverAdapter<D: FilesystemDriver>(pub D);

#[cfg(feature = "use_cached_fs")]
impl<D: FilesystemDriver> CacheFilesystemDriver for FsDriverAdapter<D> {
    fn read_file_into(
        &mut self,
        node_id: FileNodeHandle,
        out: &mut [u8],
    ) -> FileSystemResult<usize> {
        self.0
            .read_file(node_id, 0, out)
            .map_err(|_| FileSystemError::IoError)
    }

    fn write_file(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        data: &[u8],
    ) -> FileSystemResult<usize> {
        self.0
            .write_file(node_id, offset, data)
            .map_err(|_| FileSystemError::IoError)
    }

    fn get_node(&mut self, node_id: FileNodeHandle) -> FileSystemResult<FileNode> {
        self.0
            .get_node(node_id)
            .map_err(|_| FileSystemError::IoError)
    }

    fn file_size(&mut self, node_id: FileNodeHandle) -> FileSystemResult<usize> {
        self.0
            .get_node(node_id)
            .map(|n| n.size)
            .map_err(|_| FileSystemError::NotFound)
    }

    fn read_dir(&mut self, node_id: FileNodeHandle) -> FileSystemResult<Vec<String>> {
        let nodes = self
            .0
            .list_directory(node_id)
            .map_err(|_| FileSystemError::IoError)?;
        Ok(nodes.into_iter().map(|n| n.name).collect())
    }
}

#[cfg(feature = "use_cached_fs")]
pub struct CachedDriver<D: FilesystemDriver> {
    pub cache: FileCache<FsDriverAdapter<D>>,
}

#[cfg(feature = "use_cached_fs")]
impl<D: FilesystemDriver> CachedDriver<D> {
    pub fn new(inner: D, cache_size: usize) -> Self {
        Self {
            cache: FileCache::new(FsDriverAdapter(inner), cache_size),
        }
    }

    pub fn pin_file(&mut self, path: &str) -> FileSystemResult<()> {
        let node_id = self.cache.driver.0.find_node(path)?;
        self.cache.pin_file(node_id)
    }

    pub fn unpin_file(&mut self, path: &str) -> FileSystemResult<()> {
        let node_id = self.cache.driver.0.find_node(path)?;
        self.cache.unpin_file(node_id)
    }

    pub fn reserve_cache(
        &mut self,
        path: &str,
        importance: CacheImportance,
    ) -> FileSystemResult<()> {
        let node_id = self.cache.driver.0.find_node(path)?;
        self.cache.reserve_cache(node_id, importance)
    }

    pub fn evict_directory(&mut self, path: &str) -> FileSystemResult<usize> {
        let node_id = self.cache.driver.0.find_node(path)?;
        self.cache.evict_directory(node_id)
    }

    fn get_node_cached(&mut self, node_id: FileNodeHandle) -> FileSystemResult<FileNode> {
        self.cache.get_node_cached(node_id)
    }

    pub fn flush_node(&mut self, node_id: FileNodeHandle) -> FileSystemResult<()> {
        self.cache.flush_file(node_id)
    }

    pub fn flush_nodes(&mut self, node_ids: &[FileNodeHandle]) -> FileSystemResult<()> {
        self.cache.flush_nodes(node_ids)
    }

    pub fn flush_all(&mut self) -> FileSystemResult<()> {
        self.cache.flush_all()
    }

    pub fn cache_stats(&self) -> CacheStats {
        self.cache.stats()
    }
}

#[cfg(feature = "use_cached_fs")]
impl<D: FilesystemDriver> FilesystemDriver for CachedDriver<D> {
    fn read_file(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        out_buffer: &mut [u8],
    ) -> FileSystemResult<usize> {
        self.cache.read_file_range(node_id, offset, out_buffer)
    }

    fn write_file(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        data: &[u8],
    ) -> FileSystemResult<usize> {
        self.cache.write_file(node_id, offset, data)
    }

    fn find_node(&self, path: &str) -> FileSystemResult<FileNodeHandle> {
        self.cache.driver.0.find_node(path)
    }

    fn get_node(&mut self, node_id: FileNodeHandle) -> FileSystemResult<FileNode> {
        self.cache.get_node_cached(node_id)
    }

    fn list_directory(&mut self, node_id: FileNodeHandle) -> FileSystemResult<Vec<FileNode>> {
        self.cache.driver.0.list_directory(node_id)
    }

    fn create_file(
        &mut self,
        parent_id: FileNodeHandle,
        name: &str,
    ) -> FileSystemResult<FileNodeHandle> {
        self.cache.driver.0.create_file(parent_id, name)
    }

    fn create_directory(
        &mut self,
        parent_id: FileNodeHandle,
        name: &str,
    ) -> FileSystemResult<FileNodeHandle> {
        self.cache.driver.0.create_directory(parent_id, name)
    }

    fn delete(&mut self, node_id: FileNodeHandle) -> FileSystemResult<()> {
        self.cache.invalidate(node_id);
        self.cache.driver.0.delete(node_id)
    }

    fn root_node(&self) -> FileNodeHandle {
        self.cache.driver.0.root_node()
    }
}

pub struct Sirius<D: FilesystemDriver> {
    pub driver: D,
}

impl<D: FilesystemDriver> Sirius<D> {
    pub fn new(driver: D) -> Self {
        Self { driver }
    }

    pub fn resolve_path(&mut self, path: &str) -> FileSystemResult<FileNode> {
        let node_id = self.driver.find_node(path)?;
        self.driver.get_node(node_id)
    }

    pub fn open_file(&mut self, path: &str) -> FileSystemResult<FileNode> {
        let node = self.resolve_path(path)?;
        if node.file_type == FileType::File {
            Ok(node)
        } else {
            Err(FileSystemError::IsDirectory)
        }
    }

    pub fn list_directory(&mut self, path: &str) -> FileSystemResult<Vec<FileNode>> {
        let node = self.resolve_path(path)?;
        serial_println_core!(
            "Sirius: list_directory: '{}' node={:#x} type={:?}",
            path,
            node.node_id,
            node.file_type
        );
        if node.file_type == FileType::Directory {
            self.driver.list_directory(node.node_id)
        } else {
            Err(FileSystemError::NotDirectory)
        }
    }

    pub fn read_file(
        &mut self,
        path: &str,
        offset: usize,
        buffer: &mut [u8],
    ) -> FileSystemResult<usize> {
        let node = self.resolve_path(path)?;
        serial_println_core!(
            "Sirius: read_file: '{}' node={:#x} offset={} buf={}",
            path,
            node.node_id,
            offset,
            buffer.len()
        );
        if node.file_type != FileType::File {
            return Err(FileSystemError::IsDirectory);
        }
        self.driver.read_file(node.node_id, offset, buffer)
    }

    pub fn write_file(
        &mut self,
        path: &str,
        offset: usize,
        data: &[u8],
    ) -> FileSystemResult<usize> {
        let node = self.resolve_path(path)?;
        if node.file_type != FileType::File {
            return Err(FileSystemError::IsDirectory);
        }
        self.driver.write_file(node.node_id, offset, data)
    }

    pub fn create_file(&mut self, path: &str) -> FileSystemResult<FileNode> {
        let (parent_path, name) = self.split_path(path)?;
        let parent = self.resolve_path(&parent_path)?;
        serial_println_core!(
            "Sirius: create_file: '{}' parent='{}' name='{}'",
            path,
            parent_path,
            name
        );
        if parent.file_type != FileType::Directory {
            return Err(FileSystemError::NotDirectory);
        }
        let node_id = self.driver.create_file(parent.node_id, &name)?;
        serial_println_core!("Sirius: create_file: created node={:#x}", node_id);
        self.driver.get_node(node_id)
    }

    pub fn create_directory(&mut self, path: &str) -> FileSystemResult<FileNode> {
        let (parent_path, name) = self.split_path(path)?;
        let parent = self.resolve_path(&parent_path)?;
        serial_println_core!(
            "Sirius: create_directory: '{}' parent='{}' name='{}'",
            path,
            parent_path,
            name
        );
        if parent.file_type != FileType::Directory {
            return Err(FileSystemError::NotDirectory);
        }
        let node_id = self.driver.create_directory(parent.node_id, &name)?;
        serial_println_core!("Sirius: create_directory: created node={:#x}", node_id);
        self.driver.get_node(node_id)
    }

    pub fn delete(&mut self, path: &str) -> FileSystemResult<()> {
        serial_println_core!("Sirius: delete: '{}'", path);
        let node = self.resolve_path(path)?;
        self.driver.delete(node.node_id)
    }

    fn split_path(&self, path: &str) -> FileSystemResult<(String, String)> {
        if path.is_empty() || path == "/" {
            return Err(FileSystemError::InvalidPath);
        }
        let trimmed = if path.starts_with('/') {
            &path[1..]
        } else {
            path
        };
        match trimmed.rfind('/') {
            Some(pos) => {
                let parent = if pos == 0 {
                    String::from("/")
                } else {
                    String::from(&trimmed[..pos])
                };
                Ok((parent, String::from(&trimmed[pos + 1..])))
            }
            None => Ok((String::from("/"), String::from(trimmed))),
        }
    }
}

#[cfg(feature = "use_cached_fs")]
impl<D: FilesystemDriver> Sirius<CachedDriver<D>> {
    pub fn pin_file(&mut self, path: &str) -> FileSystemResult<()> {
        self.driver.pin_file(path)
    }

    pub fn unpin_file(&mut self, path: &str) -> FileSystemResult<()> {
        self.driver.unpin_file(path)
    }

    pub fn reserve_cache(
        &mut self,
        path: &str,
        importance: CacheImportance,
    ) -> FileSystemResult<()> {
        self.driver.reserve_cache(path, importance)
    }

    pub fn evict_directory(&mut self, path: &str) -> FileSystemResult<usize> {
        self.driver.evict_directory(path)
    }

    pub fn flush_node(&mut self, node_id: FileNodeHandle) -> FileSystemResult<()> {
        self.driver.flush_node(node_id)
    }

    pub fn flush_nodes(&mut self, node_ids: &[FileNodeHandle]) -> FileSystemResult<()> {
        self.driver.flush_nodes(node_ids)
    }

    pub fn flush_all(&mut self) -> FileSystemResult<()> {
        self.driver.flush_all()
    }

    pub fn cache_stats(&self) -> CacheStats {
        self.driver.cache_stats()
    }
}

#[cfg(not(feature = "use_cached_fs"))]
pub fn init_filesystem(fat32_image: &[u8]) -> Result<(), &'static str> {
    let mut disk = MockDiskDevice::new(fat32_image.len() / 512 + 1);
    disk.load_image(fat32_image)
        .map_err(|_| "Failed to load disk image")?;
    init_disk(alloc::boxed::Box::new(disk));

    let fat32_driver =
        Fat32Driver::new(&fat32_image[..512]).map_err(|_| "Failed to initialize FAT32 driver")?;

    SIRIUS.call_once(|| Mutex::new(Sirius::new(fat32_driver)));
    serial_println_core!("Filesystem initialized (no cache)");
    Ok(())
}

#[cfg(feature = "use_cached_fs")]
pub fn init_filesystem(fat32_image: &[u8]) -> Result<(), &'static str> {
    let mut disk = MockDiskDevice::new(fat32_image.len() / 512 + 1);
    disk.load_image(fat32_image)
        .map_err(|_| "Failed to load disk image")?;
    init_disk(alloc::boxed::Box::new(disk));

    let fat32_driver =
        Fat32Driver::new(&fat32_image[..512]).map_err(|_| "Failed to initialize FAT32 driver")?;

    SIRIUS.call_once(|| Mutex::new(Sirius::new(CachedDriver::new(fat32_driver, FS_CACHE_SIZE))));
    serial_println_core!("Filesystem initialized with cache size: {}", FS_CACHE_SIZE);
    Ok(())
}

#[cfg(not(feature = "use_cached_fs"))]
pub fn init_filesystem_ata() -> Result<(), &'static str> {
    let ata_driver =
        AtaPioDriver::check_primary_bus_present().ok_or("No ATA drive found on primary bus")?;
    init_disk(alloc::boxed::Box::new(ata_driver));

    let mut boot_sector_buf = [0u8; SECTOR_SIZE];
    {
        let mut disk = get_disk_mgr();
        disk.read_sector(0, &mut boot_sector_buf)
            .map_err(|_| "Failed to read boot sector from ATA drive")?;
    }

    let fat32_driver = Fat32Driver::new(&boot_sector_buf)
        .map_err(|_| "Failed to initialize FAT32 driver from ATA")?;

    SIRIUS.call_once(|| Mutex::new(Sirius::new(fat32_driver)));
    serial_println_core!("Filesystem initialized from ATA drive (no cache)");
    Ok(())
}

#[cfg(feature = "use_cached_fs")]
pub fn init_filesystem_ata() -> Result<(), &'static str> {
    init_filesystem_ata_with_cache(FS_CACHE_SIZE)
}

#[cfg(feature = "use_cached_fs")]
pub fn init_filesystem_ata_with_cache(cache_size: usize) -> Result<(), &'static str> {
    let ata_driver =
        AtaPioDriver::check_primary_bus_present().ok_or("No ATA drive found on primary bus")?;
    init_disk(alloc::boxed::Box::new(ata_driver));

    let mut boot_sector_buf = [0u8; SECTOR_SIZE];
    {
        let mut disk = get_disk_mgr();
        disk.read_sector(0, &mut boot_sector_buf)
            .map_err(|_| "Failed to read boot sector from ATA drive")?;
    }

    let fat32_driver = Fat32Driver::new(&boot_sector_buf)
        .map_err(|_| "Failed to initialize FAT32 driver from ATA")?;

    SIRIUS.call_once(|| Mutex::new(Sirius::new(CachedDriver::new(fat32_driver, cache_size))));
    serial_println_core!(
        "Filesystem initialized from ATA drive with cache size: {}",
        cache_size
    );
    Ok(())
}
