use crate::io::disk::{DiskDevice, DiskOpError, DiskOpResult, SECTOR_SIZE};
use crate::{serial_println, serial_println_core};
use x86_64::instructions::port::{Port, PortReadOnly, PortWriteOnly};

const DEBUG_LOGS: bool = true;
const DEBUG_LOGS_VERBOSE: bool = false;

macro_rules! serial_println {
    ($($arg:tt)*) => {
        if DEBUG_LOGS {
            $crate::serial_println!($($arg)*);
        }
    };
}

macro_rules! serial_println_verbose {
    ($($arg:tt)*) => {
        if DEBUG_LOGS && DEBUG_LOGS_VERBOSE {
            $crate::serial_println!($($arg)*);
        }
    };
}

const ATA_PRIMARY_DATA: u16 = 0x1F0;
const ATA_PRIMARY_ERROR: u16 = 0x1F1;
const ATA_PRIMARY_SECTOR_COUNT: u16 = 0x1F2;
const ATA_PRIMARY_LBA_LOW: u16 = 0x1F3;
const ATA_PRIMARY_LBA_MID: u16 = 0x1F4;
const ATA_PRIMARY_LBA_HIGH: u16 = 0x1F5;
const ATA_PRIMARY_DRIVE_HEAD: u16 = 0x1F6;
const ATA_PRIMARY_STATUS: u16 = 0x1F7;
const ATA_PRIMARY_COMMAND: u16 = 0x1F7;
const ATA_PRIMARY_ALT_STATUS: u16 = 0x3F6;

const STATUS_ERR: u8 = 1 << 0;
const STATUS_DRQ: u8 = 1 << 3;
const STATUS_SRV: u8 = 1 << 4;
const STATUS_DF: u8 = 1 << 5;
const STATUS_RDY: u8 = 1 << 6;
const STATUS_BSY: u8 = 1 << 7;

const CMD_READ_SECTORS: u8 = 0x20;
const CMD_WRITE_SECTORS: u8 = 0x30;
const CMD_CACHE_FLUSH: u8 = 0xE7;
const CMD_IDENTIFY: u8 = 0xEC;

// Drive/Head register: select master (drive 0) with LBA mode
const DRIVE_SELECT_MASTER_LBA: u8 = 0xE0;
const SELECT_MASTER_DRIVE: u8 = 0xA0;

const POLL_TIMEOUT_MAX_ITERATIONS: u32 = 100000;

pub struct AtaPioDriver {
    sector_count: u64,
}

impl AtaPioDriver {
    pub fn check_primary_bus_present() -> Option<Self> {
        let present = unsafe { Self::drive_identify() };
        if present {
            let sector_count = unsafe { Self::read_sector_count() };
            serial_println_core!("AtaPioDriver: found drive, sector_count={}", sector_count);
            Some(Self { sector_count })
        } else {
            serial_println_core!("AtaPioDriver: no drive found on primary bus master");
            None
        }
    }

    /// Send IDENTIFY and read back the 256-word data block
    /// Returns true if a drive responded with valid data
    unsafe fn drive_identify() -> bool {
        let mut status: PortReadOnly<u8> = PortReadOnly::new(ATA_PRIMARY_STATUS);
        let mut drive_head: Port<u8> = Port::new(ATA_PRIMARY_DRIVE_HEAD);
        let mut sector_count_port: Port<u8> = Port::new(ATA_PRIMARY_SECTOR_COUNT);
        let mut lba_low: Port<u8> = Port::new(ATA_PRIMARY_LBA_LOW);
        let mut lba_mid: Port<u8> = Port::new(ATA_PRIMARY_LBA_MID);
        let mut lba_high: Port<u8> = Port::new(ATA_PRIMARY_LBA_HIGH);
        let mut command: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_COMMAND);
        let mut data: Port<u16> = Port::new(ATA_PRIMARY_DATA);

        drive_head.write(SELECT_MASTER_DRIVE);
        sector_count_port.write(0);
        lba_low.write(0);
        lba_mid.write(0);
        lba_high.write(0);
        command.write(CMD_IDENTIFY);

        let init_read = status.read();
        if init_read == 0 {
            return false;
        }

        for _ in 0..POLL_TIMEOUT_MAX_ITERATIONS {
            let bsv_status = status.read();
            if bsv_status & STATUS_BSY == 0 {
                break;
            }
        }

        let lba_mid_val = lba_mid.read();
        let lba_high_val = lba_high.read();
        if lba_mid_val != 0 || lba_high_val != 0 {
            serial_println!(
                "AtaPioDriver: IDENTIFY: non-ATA device (LBA mid={}, high={})",
                lba_mid_val,
                lba_high_val
            );
            return false;
        }

        for _ in 0..POLL_TIMEOUT_MAX_ITERATIONS {
            let status = status.read();
            if status & STATUS_ERR != 0 {
                return false;
            }
            if status & STATUS_DRQ != 0 {
                break;
            }
        }

        let mut identify_buffer = [0u16; 256];
        for word in identify_buffer.iter_mut() {
            *word = data.read();
        }

        true
    }

    unsafe fn read_sector_count() -> u64 {
        let mut drive_head: Port<u8> = Port::new(ATA_PRIMARY_DRIVE_HEAD);
        let mut sector_count_port: Port<u8> = Port::new(ATA_PRIMARY_SECTOR_COUNT);
        let mut lba_low: Port<u8> = Port::new(ATA_PRIMARY_LBA_LOW);
        let mut lba_mid: Port<u8> = Port::new(ATA_PRIMARY_LBA_MID);
        let mut lba_high: Port<u8> = Port::new(ATA_PRIMARY_LBA_HIGH);
        let mut command: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_COMMAND);
        let mut status: PortReadOnly<u8> = PortReadOnly::new(ATA_PRIMARY_STATUS);
        let mut data: Port<u16> = Port::new(ATA_PRIMARY_DATA);

        drive_head.write(SELECT_MASTER_DRIVE);
        sector_count_port.write(0);
        lba_low.write(0);
        lba_mid.write(0);
        lba_high.write(0);
        command.write(CMD_IDENTIFY);

        for _ in 0..POLL_TIMEOUT_MAX_ITERATIONS {
            let status = status.read();
            if status & STATUS_BSY == 0 && status & STATUS_DRQ != 0 {
                break;
            }
        }

        let mut identify_buffer = [0u16; 256];
        for word in identify_buffer.iter_mut() {
            *word = data.read();
        }

        let low = identify_buffer[60] as u64;
        let high = identify_buffer[61] as u64;
        (high << 16) | low
    }

    unsafe fn poll_bsy_clear() -> DiskOpResult<()> {
        let mut alt_status: PortReadOnly<u8> = PortReadOnly::new(ATA_PRIMARY_ALT_STATUS);
        for _ in 0..POLL_TIMEOUT_MAX_ITERATIONS {
            let status = alt_status.read();
            if status & STATUS_BSY == 0 {
                return Ok(());
            }
        }
        serial_println!("AtaPioDriver: poll_bsy_clear timeout");
        Err(DiskOpError::Timeout)
    }

    unsafe fn poll_drq_set() -> DiskOpResult<()> {
        let mut alt_status: PortReadOnly<u8> = PortReadOnly::new(ATA_PRIMARY_ALT_STATUS);
        for _ in 0..POLL_TIMEOUT_MAX_ITERATIONS {
            let status = alt_status.read();
            if status & STATUS_ERR != 0 || status & STATUS_DF != 0 {
                serial_println!(
                    "AtaPioDriver: drive error during poll_drq_set, status={:#x}",
                    status
                );
                return Err(DiskOpError::ReadError);
            }
            if status & STATUS_DRQ != 0 {
                return Ok(());
            }
        }
        serial_println!("AtaPioDriver: poll_drq_set timeout");
        Err(DiskOpError::Timeout)
    }

    unsafe fn select_drive_lba28(lba: u64) {
        let mut drive_head: Port<u8> = Port::new(ATA_PRIMARY_DRIVE_HEAD);
        let lba_top = ((lba >> 24) & 0x0F) as u8;
        drive_head.write(DRIVE_SELECT_MASTER_LBA | lba_top);
    }

    /// Single-sector PIO read into a 512-byte slice
    unsafe fn read_sector_pio(lba: u64, buf: &mut [u8]) -> DiskOpResult<()> {
        debug_assert!(buf.len() >= SECTOR_SIZE);
        debug_assert!(lba < (1u64 << 28));

        Self::poll_bsy_clear()?;
        Self::select_drive_lba28(lba);

        let mut sector_count_port: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_SECTOR_COUNT);
        let mut lba_low: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_LBA_LOW);
        let mut lba_mid: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_LBA_MID);
        let mut lba_high: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_LBA_HIGH);
        let mut command: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_COMMAND);
        let mut data: Port<u16> = Port::new(ATA_PRIMARY_DATA);

        sector_count_port.write(1);
        lba_low.write((lba & 0xFF) as u8);
        lba_mid.write(((lba >> 8) & 0xFF) as u8);
        lba_high.write(((lba >> 16) & 0xFF) as u8);
        command.write(CMD_READ_SECTORS);

        Self::poll_drq_set()?;

        let words = SECTOR_SIZE / 2;
        for i in 0..words {
            let word = data.read();
            let offset = i * 2;
            buf[offset] = (word & 0xFF) as u8;
            buf[offset + 1] = ((word >> 8) & 0xFF) as u8;
        }

        Ok(())
    }

    /// Single-sector PIO write from a 512-byte slice
    /// flush cache after
    unsafe fn write_sector_pio(lba: u64, buf: &[u8]) -> DiskOpResult<()> {
        debug_assert!(buf.len() >= SECTOR_SIZE);
        debug_assert!(lba < (1u64 << 28));

        Self::poll_bsy_clear()?;
        Self::select_drive_lba28(lba);

        let mut sector_count_port: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_SECTOR_COUNT);
        let mut lba_low: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_LBA_LOW);
        let mut lba_mid: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_LBA_MID);
        let mut lba_high: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_LBA_HIGH);
        let mut command: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_COMMAND);
        let mut data: Port<u16> = Port::new(ATA_PRIMARY_DATA);

        sector_count_port.write(1);
        lba_low.write((lba & 0xFF) as u8);
        lba_mid.write(((lba >> 8) & 0xFF) as u8);
        lba_high.write(((lba >> 16) & 0xFF) as u8);
        command.write(CMD_WRITE_SECTORS);

        Self::poll_drq_set()?;

        let words = SECTOR_SIZE / 2;
        for i in 0..words {
            let offset = i * 2;
            let word = (buf[offset] as u16) | ((buf[offset + 1] as u16) << 8);
            data.write(word);
        }

        let mut flush_cmd: PortWriteOnly<u8> = PortWriteOnly::new(ATA_PRIMARY_COMMAND);
        flush_cmd.write(CMD_CACHE_FLUSH);
        Self::poll_bsy_clear()?;

        Ok(())
    }
}

impl DiskDevice for AtaPioDriver {
    fn read_sectors(
        &mut self,
        start_sector: u64,
        count: usize,
        out_buffer: &mut [u8],
    ) -> DiskOpResult<()> {
        if start_sector + count as u64 > self.sector_count {
            return Err(DiskOpError::InvalidSector);
        }
        if out_buffer.len() < count * SECTOR_SIZE {
            return Err(DiskOpError::BufferTooSmall);
        }

        for i in 0..count {
            let lba = start_sector + i as u64;
            let offset = i * SECTOR_SIZE;
            unsafe {
                Self::read_sector_pio(lba, &mut out_buffer[offset..offset + SECTOR_SIZE])?;
            }
        }
        Ok(())
    }

    fn write_sectors(&mut self, start_sector: u64, count: usize, data: &[u8]) -> DiskOpResult<()> {
        serial_println_verbose!(
            "AtaPioDriver: write_sectors called with start_sector={}, count={}, data_len={}",
            start_sector,
            count,
            data.len()
        );
        if start_sector + count as u64 > self.sector_count {
            return Err(DiskOpError::InvalidSector);
        }
        if data.len() < count * SECTOR_SIZE {
            return Err(DiskOpError::BufferTooSmall);
        }

        for i in 0..count {
            let lba = start_sector + i as u64;
            let offset = i * SECTOR_SIZE;
            unsafe {
                Self::write_sector_pio(lba, &data[offset..offset + SECTOR_SIZE])?;
            }
        }
        Ok(())
    }

    fn sector_count(&self) -> u64 {
        self.sector_count
    }
}
