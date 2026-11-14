use crate::filesystem::fat32::Fat32Driver;
use crate::filesystem::fat32::FileNodeHandle;
use crate::io::disk::{BlockDevice, DiskOpError};
use alloc::string::String;
use alloc::vec::Vec;
use bitflags::bitflags;
use spin::{Mutex, Once};

pub static VFS: Once<Mutex<Vfs>> = Once::new();

pub fn get_vfs() -> spin::MutexGuard<'static, Vfs> {
    VFS.get().unwrap().lock()
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
        //NOTE: actually careful with 8th bit, modify the node_id packing code
        const RESERVED = 0b11110000;
    }
}

impl FileAttributes {
    pub const FILE_READONLY: Self = Self::READ;
    pub const FILE_READ_WRITE: Self = Self::READ.union(Self::WRITE);
    pub const FILE_READ_WRITE_EXECUTE: Self = Self::READ.union(Self::WRITE).union(Self::EXECUTE);

    pub const DIR_DEFAULT: Self = Self::READ;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    NotSupported,
    /// write starting beyond the current end of the file
    OffsetPastEnd,
}

impl From<DiskOpError> for FileSystemError {
    fn from(_value: DiskOpError) -> Self {
        FileSystemError::DiskOpError
    }
}

pub struct Vfs {
    pub driver: Fat32Driver,
}

impl Vfs {
    pub fn new(driver: Fat32Driver) -> Self {
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
        let node = self.open_file(path)?;

        self.read_node(node.node_id, offset, buffer)
    }

    pub fn write_file(
        &mut self,
        path: &str,
        offset: usize,
        data: &[u8],
    ) -> FileSystemResult<usize> {
        let node = self.open_file(path)?;

        self.write_node(node.node_id, offset, data)
    }

    /// Read through a node handle, as kept by an open file descriptor
    pub fn read_node(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        buffer: &mut [u8],
    ) -> FileSystemResult<usize> {
        self.driver.read_file(node_id, offset, buffer)
    }

    pub fn write_node(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        data: &[u8],
    ) -> FileSystemResult<usize> {
        let written = self.driver.write_file(node_id, offset, data)?;
        self.driver.device.flush()?;

        Ok(written)
    }

    pub fn create_file(&mut self, path: &str) -> FileSystemResult<FileNode> {
        let (parent_path, name) = split_path(path)?;
        let parent = self.resolve_path(parent_path)?;
        if parent.file_type != FileType::Directory {
            return Err(FileSystemError::NotDirectory);
        }

        let node_id = self.driver.create_file(parent.node_id, name)?;
        self.driver.device.flush()?;

        self.driver.get_node(node_id)
    }

    pub fn create_directory(&mut self, path: &str) -> FileSystemResult<FileNode> {
        let (parent_path, name) = split_path(path)?;
        let parent = self.resolve_path(parent_path)?;
        if parent.file_type != FileType::Directory {
            return Err(FileSystemError::NotDirectory);
        }

        let node_id = self.driver.create_directory(parent.node_id, name)?;
        self.driver.device.flush()?;

        self.driver.get_node(node_id)
    }

    pub fn delete(&mut self, path: &str) -> FileSystemResult<()> {
        let node = self.resolve_path(path)?;

        self.driver.delete(node.node_id)?;
        self.driver.device.flush()?;

        Ok(())
    }
}

/// splits a path into the parent directory part and the final name
pub fn split_path(path: &str) -> FileSystemResult<(&str, &str)> {
    let path = path.strip_prefix('/').unwrap_or(path);
    let path = path.strip_suffix('/').unwrap_or(path);
    if path.is_empty() {
        return Err(FileSystemError::InvalidPath);
    }

    match path.rfind('/') {
        Some(separator_index) => Ok((&path[..separator_index], &path[separator_index + 1..])),
        None => Ok(("/", path)),
    }
}

pub fn init_filesystem(device: BlockDevice) -> FileSystemResult<()> {
    let driver = Fat32Driver::new(device)?;
    VFS.call_once(|| Mutex::new(Vfs::new(driver)));

    Ok(())
}
