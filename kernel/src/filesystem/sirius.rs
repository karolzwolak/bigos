use crate::filesystem::fat32::Fat32Driver;
use crate::filesystem::fat32::FileNodeHandle;
use crate::io::disk::{DiskOpError, MockDiskDevice, init_disk};
use crate::serial_println;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use bitflags::bitflags;
use lazy_static::lazy_static;
use spin::{Mutex, Once};

lazy_static! {
    pub static ref SIRIUS: Once<Mutex<Sirius>> = Once::new();
}

pub fn get_sirius() -> spin::MutexGuard<'static, Sirius> {
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
    NotSupported,
}

impl From<DiskOpError> for FileSystemError {
    fn from(_value: DiskOpError) -> Self {
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
    fn get_node(&self, node_id: FileNodeHandle) -> FileSystemResult<FileNode>;

    fn list_directory(&self, node_id: FileNodeHandle) -> FileSystemResult<Vec<FileNode>>;

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

pub struct Sirius {
    driver: Box<dyn FilesystemDriver>,
}

impl Sirius {
    pub fn new(driver: Box<dyn FilesystemDriver>) -> Self {
        Self { driver }
    }

    pub fn resolve_path(&self, path: &str) -> FileSystemResult<FileNode> {
        let node_id = self.driver.find_node(path)?;
        self.driver.get_node(node_id)
    }

    pub fn open_file(&self, path: &str) -> FileSystemResult<FileNode> {
        let node = self.resolve_path(path)?;
        if node.file_type == FileType::File {
            Ok(node)
        } else {
            Err(FileSystemError::IsDirectory)
        }
    }

    pub fn list_directory(&self, path: &str) -> FileSystemResult<Vec<FileNode>> {
        let node = self.resolve_path(path)?;
        serial_println!(
            "Resolved path '{}' to node ID {:#x}, type: {:?}",
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
        serial_println!(
            "Reading file '{}' (node ID {:#x}) at offset {}, buffer size {}",
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
        let parent = self.resolve_path(parent_path.as_str())?;

        serial_println!(
            "Sirius: create_file: path: {}, parent_path: {}, name: {}",
            path,
            parent_path,
            name
        );

        if parent.file_type != FileType::Directory {
            serial_println!("Error: Sirius: create_file: parent is not a directory");
            return Err(FileSystemError::NotDirectory);
        }

        let node_id = self.driver.create_file(parent.node_id, name.as_str())?;
        serial_println!("Sirius: create_file: created file: {:#x}", node_id);

        self.driver.get_node(node_id)
    }

    pub fn create_directory(&mut self, path: &str) -> FileSystemResult<FileNode> {
        let (parent_path, name) = self.split_path(path)?;
        let parent = self.resolve_path(parent_path.as_str())?;

        serial_println!(
            "Sirius: create_directory: path: {}, parent_path: {}, name: {}",
            path,
            parent_path,
            name
        );

        if parent.file_type != FileType::Directory {
            return Err(FileSystemError::NotDirectory);
        }

        let node_id = self
            .driver
            .create_directory(parent.node_id, name.as_str())?;
        serial_println!(
            "Sirius: create_directory: created directory: {:#x}",
            node_id
        );

        self.driver.get_node(node_id)
    }

    pub fn delete(&mut self, path: &str) -> FileSystemResult<()> {
        serial_println!("Sirius: delete: looking for path: {}", path);
        let node = self.resolve_path(path)?;

        serial_println!(
            "Sirius: delete: path: {}, resolved node: {}",
            path,
            node.name
        );

        self.driver.delete(node.node_id)
    }

    // Split path into parents part and filename
    fn split_path(&self, path: &str) -> FileSystemResult<(String, String)> {
        if path.is_empty() || path == "/" {
            return Err(FileSystemError::InvalidPath);
        }

        let path = path.strip_prefix('/').unwrap_or(path);

        // find the last '/', the parent part is everything before it
        match path.rfind('/') {
            Some(pos) => {
                let parent = if pos == 0 {
                    String::from("/")
                } else {
                    String::from(&path[..pos])
                };
                let name = String::from(&path[pos + 1..]);
                Ok((parent, name))
            }
            None => Ok((String::from("/"), String::from(path))),
        }
    }
}

pub fn init_filesystem(fat32_image: &[u8]) -> Result<(), &'static str> {
    let mut disk = MockDiskDevice::new(fat32_image.len() / 512 + 1);
    disk.load_image(fat32_image)
        .map_err(|_| "Failed to load disk image")?;
    init_disk(Box::new(disk));

    let boot_sector_data = &fat32_image[..512];

    let fat32_driver =
        Fat32Driver::new(boot_sector_data).map_err(|_| "Failed to initialize FAT32 driver")?;

    SIRIUS.call_once(|| Mutex::new(Sirius::new(Box::new(fat32_driver))));

    Ok(())
}
