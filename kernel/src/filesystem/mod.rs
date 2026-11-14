pub mod fat32;
pub mod vfs;

pub use vfs::{FileNode, FileType, VFS, Vfs, get_vfs, init_filesystem};

use crate::io::disk::BlockDevice;
use crate::io::virtio::VirtioBlk;
use crate::serial_println;

pub fn init_boot_filesystem() {
    let device = match VirtioBlk::find_and_init() {
        Ok(device) => device,
        Err(error) => {
            serial_println!(
                "warning: no virtio-blk disk ({:?}), booting without a filesystem",
                error
            );
            return;
        }
    };

    if let Err(error) = init_filesystem(BlockDevice::VirtioBlk(device)) {
        serial_println!(
            "warning: mounting the disk failed ({:?}), booting without a filesystem",
            error
        );
        return;
    }

    let mut vfs = get_vfs();
    let label = vfs.driver.boot_sector.volume_label;
    serial_println!(
        "Mounted FAT32 volume \"{}\"",
        core::str::from_utf8(&label).unwrap_or("?").trim_end()
    );

    match vfs.list_directory("/") {
        Ok(entries) => {
            for entry in entries {
                serial_println!(
                    "  {:?} {} ({} bytes)",
                    entry.file_type,
                    entry.name,
                    entry.size
                );
            }
        }
        Err(error) => serial_println!("warning: listing the root failed ({:?})", error),
    }
}
