use crate::filesystem::{get_sirius, init_filesystem_ata};
use crate::io::ata::AtaPioDriver;
use crate::io::disk::{DiskDevice, SECTOR_SIZE};
use crate::serial_println;

// Sector used for the raw write/read-back test, past the FAT32 reserved/FAT regions for a 16MB disk
const TEST_SECTOR_LBA: u64 = 100;

const COUNTER_FILE: &str = "/counter.txt";

pub fn test_ata() {
    serial_println!("--- ATA PIO test begin ---");

    let mut driver = match AtaPioDriver::check_primary_bus_present() {
        Some(d) => d,
        None => {
            serial_println!(
                "FAIL: AtaPioDriver::check_primary_bus_present() returned None - no ATA drive detected"
            );
            return;
        }
    };

    serial_println!(
        "OK: drive detected, {} sectors total",
        driver.sector_count()
    );

    test_read_sector0(&mut driver);
    test_write_read_back(&mut driver);

    serial_println!("--- ATA PIO test end ---");
}

pub fn test_ata_filesystem() {
    serial_println!("--- ATA filesystem persistence test begin ---");

    test_boot_counter();

    serial_println!("--- ATA filesystem persistence test end ---");
}

// Read sector 0 and verify the FAT32 boot signature
fn test_read_sector0(driver: &mut AtaPioDriver) {
    let mut buf = [0u8; SECTOR_SIZE];

    match driver.read_sectors(0, 1, &mut buf) {
        Ok(()) => {
            let sig_ok = buf[510] == 0x55 && buf[511] == 0xAA;
            if sig_ok {
                serial_println!("OK: sector 0 read - FAT32 boot signature 0x55AA present");
            } else {
                serial_println!(
                    "WARN: sector 0 boot signature is {:#x} {:#x} (expected 0x55 0xAA)",
                    buf[510],
                    buf[511]
                );
            }
            serial_println!(
                "      sector 0 bytes[0..16]: {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}  {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}",
                buf[0],
                buf[1],
                buf[2],
                buf[3],
                buf[4],
                buf[5],
                buf[6],
                buf[7],
                buf[8],
                buf[9],
                buf[10],
                buf[11],
                buf[12],
                buf[13],
                buf[14],
                buf[15]
            );
        }
        Err(e) => serial_println!("FAIL: sector 0 read error: {:?}", e),
    }
}

// Write a known pattern to sector 100, read it back, compare every byte
fn test_write_read_back(driver: &mut AtaPioDriver) {
    let mut write_buf = [0u8; SECTOR_SIZE];
    for (i, byte) in write_buf.iter_mut().enumerate() {
        *byte = (i & 0xFF) as u8;
    }
    write_buf[0] = 0xDE;
    write_buf[1] = 0xAD;
    write_buf[2] = 0xC0;
    write_buf[3] = 0xDE;

    match driver.write_sectors(TEST_SECTOR_LBA, 1, &write_buf) {
        Ok(()) => serial_println!("OK: wrote pattern to sector {}", TEST_SECTOR_LBA),
        Err(e) => {
            serial_println!("FAIL: write to sector {} failed: {:?}", TEST_SECTOR_LBA, e);
            return;
        }
    }

    let mut read_buf = [0u8; SECTOR_SIZE];
    match driver.read_sectors(TEST_SECTOR_LBA, 1, &mut read_buf) {
        Ok(()) => serial_println!("OK: read back sector {}", TEST_SECTOR_LBA),
        Err(e) => {
            serial_println!("FAIL: read from sector {} failed: {:?}", TEST_SECTOR_LBA, e);
            return;
        }
    }

    let mut mismatch = false;
    for i in 0..SECTOR_SIZE {
        if read_buf[i] != write_buf[i] {
            serial_println!(
                "FAIL: mismatch at byte {} - wrote {:#x} read {:#x}",
                i,
                write_buf[i],
                read_buf[i]
            );
            mismatch = true;
            break;
        }
    }

    if !mismatch {
        serial_println!(
            "OK: write/read-back verified - all {} bytes match",
            SECTOR_SIZE
        );
    }
}

// Read /counter.txt from the FAT32 volume, parse the integer, increment it,
// write back, and read again to confirm. On first boot the file is absent and is created with value 0
fn test_boot_counter() {
    let mut sirius = get_sirius();

    let node = match sirius.open_file(COUNTER_FILE) {
        Ok(n) => {
            serial_println!("OK: {} found ({} bytes)", COUNTER_FILE, n.size);
            n
        }
        Err(crate::filesystem::sirius::FileSystemError::NotFound) => {
            serial_println!("INFO: {} not found - creating", COUNTER_FILE);
            match sirius.create_file(COUNTER_FILE) {
                Ok(n) => {
                    serial_println!("OK: created {}", COUNTER_FILE);
                    n
                }
                Err(e) => {
                    serial_println!("FAIL: could not create {}: {:?}", COUNTER_FILE, e);
                    return;
                }
            }
        }
        Err(e) => {
            serial_println!("FAIL: open {} error: {:?}", COUNTER_FILE, e);
            return;
        }
    };

    let mut read_buf = [0u8; 16];
    let bytes_read = if node.size > 0 {
        match sirius.read_file(COUNTER_FILE, 0, &mut read_buf) {
            Ok(n) => n,
            Err(e) => {
                serial_println!("FAIL: read {} error: {:?}", COUNTER_FILE, e);
                return;
            }
        }
    } else {
        0
    };

    let old_value: u64 = if bytes_read > 0 {
        core::str::from_utf8(&read_buf[..bytes_read])
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    } else {
        0
    };

    serial_println!("INFO: boot counter before = {}", old_value);

    let new_value = old_value + 1;

    let new_str = fmt_u64(new_value);
    let new_bytes = new_str.as_bytes();

    match sirius.write_file(COUNTER_FILE, 0, new_bytes) {
        Ok(n) => serial_println!("OK: wrote {} bytes to {}", n, COUNTER_FILE),
        Err(e) => {
            serial_println!("FAIL: write {} error: {:?}", COUNTER_FILE, e);
            return;
        }
    }

    let mut verify_buf = [0u8; 16];
    match sirius.read_file(COUNTER_FILE, 0, &mut verify_buf) {
        Ok(n) => {
            let readback = core::str::from_utf8(&verify_buf[..n])
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(u64::MAX);

            if readback == new_value {
                serial_println!(
                    "OK: boot counter after write = {} (verified read-back)",
                    readback
                );
                serial_println!("INFO: next boot should show counter before = {}", new_value);
            } else {
                serial_println!(
                    "FAIL: read-back mismatch - wrote {} read back {}",
                    new_value,
                    readback
                );
            }
        }
        Err(e) => serial_println!("FAIL: verify read error: {:?}", e),
    }
}

fn fmt_u64(mut n: u64) -> FmtBuf {
    let mut buf = FmtBuf {
        data: [0u8; 20],
        len: 0,
    };
    if n == 0 {
        buf.data[0] = b'0';
        buf.len = 1;
        return buf;
    }
    let mut tmp = [0u8; 20];
    let mut i = 0;
    while n > 0 {
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    // tmp holds digits in reverse order
    for j in 0..i {
        buf.data[j] = tmp[i - 1 - j];
    }
    buf.len = i;
    buf
}

struct FmtBuf {
    data: [u8; 20],
    len: usize,
}

impl FmtBuf {
    fn as_bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }
}
