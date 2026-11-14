pub mod fat32;
pub mod vfs;

pub use vfs::{FileNode, FileType, VFS, Vfs, get_vfs, init_filesystem};
