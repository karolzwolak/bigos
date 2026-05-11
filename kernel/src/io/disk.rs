use crate::serial_println;
use alloc::boxed::Box;
use alloc::vec::Vec;
use spin::{Mutex, Once};

const DISK_OP_DEBUG: bool = false;
macro_rules! disk_op_debug {
    ($($arg:tt)*) => {
        if DISK_OP_DEBUG {
            serial_println!($($arg)*);
        }
    };
}

pub const SECTOR_SIZE: usize = 512;

pub type DiskOpResult<T> = Result<T, DiskOpError>;

#[derive(Debug, Clone, Copy)]
pub enum DiskOpError {
    ReadError,
    WriteError,
    InvalidSector,
    DeviceNotFound,
    BufferTooSmall,
    Timeout,
}

pub trait DiskDevice {
    /// Read n sectors into buffer
    fn read_sectors(
        &mut self,
        start_sector: u64,
        count: usize,
        out_buffer: &mut [u8],
    ) -> DiskOpResult<()>;

    /// Write n sectors from buffer
    fn write_sectors(&mut self, start_sector: u64, count: usize, data: &[u8]) -> DiskOpResult<()>;

    fn sector_count(&self) -> u64;
}

/// testing device using an in-memory buffer
pub struct MockDiskDevice {
    size_bytes: usize,
    data: Vec<u8>,
}

impl MockDiskDevice {
    pub fn new(size_sectors: usize) -> Self {
        let size_bytes = size_sectors * SECTOR_SIZE;
        Self {
            size_bytes,
            data: alloc::vec![0u8; size_bytes],
        }
    }

    pub fn load_image(&mut self, image_data: &[u8]) -> DiskOpResult<()> {
        if image_data.len() > self.size_bytes {
            return Err(DiskOpError::InvalidSector);
        }
        self.data[..image_data.len()].copy_from_slice(image_data);
        serial_println!(
            "MockDiskDevice: Loaded image of size {} bytes",
            image_data.len()
        );
        Ok(())
    }
}

impl DiskDevice for MockDiskDevice {
    fn read_sectors(
        &mut self,
        start_sector: u64,
        count: usize,
        out_buffer: &mut [u8],
    ) -> DiskOpResult<()> {
        let start_byte = (start_sector as usize) * SECTOR_SIZE;
        let end_byte = start_byte + (count * SECTOR_SIZE);
        disk_op_debug!(
            "MockDiskDevice: Reading sectors from {} to {}",
            start_sector,
            start_sector + count as u64
        );

        if end_byte > self.size_bytes {
            disk_op_debug!(
                "MockDiskDevice: Read error - end byte {} exceeds disk size {}",
                end_byte,
                self.size_bytes
            );
            return Err(DiskOpError::InvalidSector);
        }

        if out_buffer.len() < count * SECTOR_SIZE {
            disk_op_debug!(
                "MockDiskDevice: Read error - out_buffer size {} is too small for {} sectors",
                out_buffer.len(),
                count
            );
            return Err(DiskOpError::InvalidSector);
        }

        out_buffer[..count * SECTOR_SIZE].copy_from_slice(&self.data[start_byte..end_byte]);
        disk_op_debug!(
            "MockDiskDevice: Successfully read {} bytes",
            count * SECTOR_SIZE
        );
        Ok(())
    }

    fn write_sectors(&mut self, start_sector: u64, count: usize, data: &[u8]) -> DiskOpResult<()> {
        let start_byte = (start_sector as usize) * SECTOR_SIZE;
        let byte_count = count * SECTOR_SIZE;
        let end_byte = start_byte + byte_count;

        if end_byte > self.size_bytes {
            return Err(DiskOpError::InvalidSector);
        }

        if data.len() < byte_count {
            return Err(DiskOpError::InvalidSector);
        }

        disk_op_debug!(
            "MockDiskDevice: write_sectors: start_byte: {}, byte_count: {}, end_byte: {}",
            start_byte,
            byte_count,
            end_byte
        );
        self.data[start_byte..end_byte].copy_from_slice(&data[..byte_count]);
        disk_op_debug!("MockDiskDevice: write_sectors: finished");
        Ok(())
    }

    fn sector_count(&self) -> u64 {
        (self.size_bytes / SECTOR_SIZE) as u64
    }
}

pub struct DiskManager {
    device: Box<dyn DiskDevice>,
}
unsafe impl Send for DiskManager {}

impl DiskManager {
    pub fn new(device: Box<dyn DiskDevice>) -> Self {
        Self { device }
    }

    pub fn read_sector(&mut self, sector: u64, out_buffer: &mut [u8]) -> DiskOpResult<()> {
        if out_buffer.len() < SECTOR_SIZE {
            return Err(DiskOpError::InvalidSector);
        }
        self.device.read_sectors(sector, 1, out_buffer)
    }

    pub fn read_sectors(
        &mut self,
        start: u64,
        count: usize,
        out_buffer: &mut [u8],
    ) -> DiskOpResult<()> {
        self.device.read_sectors(start, count, out_buffer)
    }

    pub fn write_sector(&mut self, sector: u64, data: &[u8]) -> DiskOpResult<()> {
        if data.len() < SECTOR_SIZE {
            return Err(DiskOpError::InvalidSector);
        }
        self.device.write_sectors(sector, 1, data)
    }

    pub fn write_sectors(&mut self, sector: u64, count: usize, data: &[u8]) -> DiskOpResult<()> {
        if data.len() < SECTOR_SIZE {
            return Err(DiskOpError::InvalidSector);
        }
        self.device.write_sectors(sector, count, data)
    }

    pub fn sector_count(&self) -> u64 {
        self.device.sector_count()
    }
}

use lazy_static::lazy_static;

lazy_static! {
    pub static ref DISK: Once<Mutex<DiskManager>> = Once::new();
}

pub fn init_disk(device: Box<dyn DiskDevice>) {
    DISK.call_once(|| Mutex::new(DiskManager::new(device)));
}

pub fn get_disk_mgr() -> spin::MutexGuard<'static, DiskManager> {
    DISK.get().unwrap().lock()
}
