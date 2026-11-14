use crate::serial_println;
use alloc::vec::Vec;

pub use crate::io::virtio::VirtioBlk;

pub const SECTOR_SIZE_BYTES: usize = 512;

pub type DiskOpResult<T> = Result<T, DiskOpError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskOpError {
    ReadError,
    WriteError,
    InvalidSector,
    DeviceNotFound,
    BufferTooSmall,
    Timeout,
}

/// in-memory disk used by tests
pub struct RamDisk {
    pub data: Vec<u8>,
}

impl RamDisk {
    pub fn new(size_sectors: usize) -> Self {
        Self {
            data: alloc::vec![0u8; size_sectors * SECTOR_SIZE_BYTES],
        }
    }

    pub fn from_image(image_data: &[u8]) -> Self {
        let size_sectors = image_data.len().div_ceil(SECTOR_SIZE_BYTES);
        let mut disk = Self::new(size_sectors);
        disk.data[..image_data.len()].copy_from_slice(image_data);
        serial_println!("RamDisk: loaded image of {} bytes", image_data.len());

        disk
    }

    pub fn sector_count(&self) -> u64 {
        (self.data.len() / SECTOR_SIZE_BYTES) as u64
    }

    pub fn read_sectors(&self, start_sector: u64, buffer: &mut [u8]) -> DiskOpResult<()> {
        let start_byte = start_sector as usize * SECTOR_SIZE_BYTES;
        let end_byte = start_byte + buffer.len();
        if end_byte > self.data.len() {
            return Err(DiskOpError::InvalidSector);
        }

        buffer.copy_from_slice(&self.data[start_byte..end_byte]);

        Ok(())
    }

    pub fn write_sectors(&mut self, start_sector: u64, buffer: &[u8]) -> DiskOpResult<()> {
        let start_byte = start_sector as usize * SECTOR_SIZE_BYTES;
        let end_byte = start_byte + buffer.len();
        if end_byte > self.data.len() {
            return Err(DiskOpError::InvalidSector);
        }

        self.data[start_byte..end_byte].copy_from_slice(buffer);

        Ok(())
    }
}

#[allow(clippy::large_enum_variant)]
pub enum BlockDevice {
    Ram(RamDisk),
    VirtioBlk(VirtioBlk),
}

impl BlockDevice {
    pub fn sector_count(&self) -> u64 {
        match self {
            BlockDevice::Ram(disk) => disk.sector_count(),
            BlockDevice::VirtioBlk(disk) => disk.sector_count(),
        }
    }

    /// buffer length must be a multiple of the sector size
    pub fn read_sectors(&mut self, start_sector: u64, buffer: &mut [u8]) -> DiskOpResult<()> {
        debug_assert!(buffer.len().is_multiple_of(SECTOR_SIZE_BYTES));

        match self {
            BlockDevice::Ram(disk) => disk.read_sectors(start_sector, buffer),
            BlockDevice::VirtioBlk(disk) => disk.read_sectors(start_sector, buffer),
        }
    }

    /// buffer length must be a multiple of the sector size
    pub fn write_sectors(&mut self, start_sector: u64, buffer: &[u8]) -> DiskOpResult<()> {
        debug_assert!(buffer.len().is_multiple_of(SECTOR_SIZE_BYTES));

        match self {
            BlockDevice::Ram(disk) => disk.write_sectors(start_sector, buffer),
            BlockDevice::VirtioBlk(disk) => disk.write_sectors(start_sector, buffer),
        }
    }

    /// makes completed writes durable, nothing to do for a RAM disk
    pub fn flush(&mut self) -> DiskOpResult<()> {
        match self {
            BlockDevice::Ram(_) => Ok(()),
            BlockDevice::VirtioBlk(disk) => disk.flush(),
        }
    }
}
