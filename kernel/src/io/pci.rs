use spin::Mutex;
use x86_64::instructions::port::Port;

pub const PCI_CONFIG_ADDRESS_PORT: u16 = 0xCF8;
pub const PCI_CONFIG_DATA_PORT: u16 = 0xCFC;

const CONFIG_ENABLE_BIT: u32 = 1 << 31;
const CONFIG_BUS_SHIFT: u32 = 16;
const CONFIG_DEVICE_SHIFT: u32 = 11;
const CONFIG_FUNCTION_SHIFT: u32 = 8;
const CONFIG_OFFSET_MASK: u32 = 0xFC;

pub const BUS_COUNT: u16 = 256;
pub const DEVICES_PER_BUS: u8 = 32;
pub const FUNCTIONS_PER_DEVICE: u8 = 8;
pub const BAR_COUNT: u8 = 6;

pub const OFFSET_VENDOR_DEVICE_ID: u8 = 0x00;
pub const OFFSET_COMMAND_STATUS: u8 = 0x04;
pub const OFFSET_HEADER_TYPE: u8 = 0x0C;
pub const OFFSET_BAR0: u8 = 0x10;
pub const BAR_STRIDE_BYTES: u8 = 4;

pub const VENDOR_ID_MASK: u32 = 0xFFFF;
pub const DEVICE_ID_SHIFT: u32 = 16;
pub const INVALID_VENDOR_ID: u16 = 0xFFFF;

pub const HEADER_TYPE_SHIFT: u32 = 16;
pub const HEADER_TYPE_MULTIFUNCTION_BIT: u32 = 1 << 7;

pub const COMMAND_IO_SPACE: u32 = 1 << 0;
pub const COMMAND_MEMORY_SPACE: u32 = 1 << 1;
pub const COMMAND_BUS_MASTER: u32 = 1 << 2;
pub const COMMAND_MASK: u32 = 0xFFFF;

const BAR_IO_SPACE_BIT: u32 = 1 << 0;
const BAR_MEMORY_TYPE_MASK: u32 = 0b110;
const BAR_MEMORY_TYPE_64_BIT: u32 = 0b100;
const BAR_PREFETCHABLE_BIT: u32 = 1 << 3;
const BAR_MEMORY_ADDRESS_MASK: u32 = 0xFFFF_FFF0;
const BAR_IO_ADDRESS_MASK: u32 = 0xFFFF_FFFC;
const BAR_SIZING_PATTERN: u32 = 0xFFFF_FFFF;

pub const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
pub const VIRTIO_BLK_MODERN_DEVICE_ID: u16 = 0x1042;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciLocation {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciLocation {
    pub const fn new(bus: u8, device: u8, function: u8) -> Self {
        debug_assert!(device < DEVICES_PER_BUS);
        debug_assert!(function < FUNCTIONS_PER_DEVICE);

        Self {
            bus,
            device,
            function,
        }
    }

    const fn config_address(&self, register_offset: u8) -> u32 {
        CONFIG_ENABLE_BIT
            | ((self.bus as u32) << CONFIG_BUS_SHIFT)
            | ((self.device as u32) << CONFIG_DEVICE_SHIFT)
            | ((self.function as u32) << CONFIG_FUNCTION_SHIFT)
            | (register_offset as u32 & CONFIG_OFFSET_MASK)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciBar {
    Unused,
    Memory {
        address: u64,
        size_bytes: u64,
        is_64_bit: bool,
        is_prefetchable: bool,
    },
    Io {
        port_base: u32,
        size_bytes: u32,
    },
}

impl PciBar {
    pub const fn takes_two_slots(&self) -> bool {
        matches!(
            self,
            PciBar::Memory {
                is_64_bit: true,
                ..
            }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciDeviceInfo {
    pub location: PciLocation,
    pub vendor_id: u16,
    pub device_id: u16,
}

struct ConfigPorts {
    address: Port<u32>,
    data: Port<u32>,
}

static CONFIG_PORTS: Mutex<ConfigPorts> = Mutex::new(ConfigPorts {
    address: Port::new(PCI_CONFIG_ADDRESS_PORT),
    data: Port::new(PCI_CONFIG_DATA_PORT),
});

pub fn config_read_dword(location: PciLocation, register_offset: u8) -> u32 {
    let mut ports = CONFIG_PORTS.lock();

    // SAFETY: 0xCF8 and 0xCFC are the legacy PCI configuration ports
    unsafe {
        ports
            .address
            .write(location.config_address(register_offset));
        ports.data.read()
    }
}

pub fn config_write_dword(location: PciLocation, register_offset: u8, value: u32) {
    let mut ports = CONFIG_PORTS.lock();

    // SAFETY: 0xCF8 and 0xCFC are the legacy PCI configuration ports
    unsafe {
        ports
            .address
            .write(location.config_address(register_offset));
        ports.data.write(value);
    }
}

pub fn read_vendor_and_device(location: PciLocation) -> (u16, u16) {
    let value = config_read_dword(location, OFFSET_VENDOR_DEVICE_ID);

    (
        (value & VENDOR_ID_MASK) as u16,
        (value >> DEVICE_ID_SHIFT) as u16,
    )
}

/// Scans bus 0 first, then the remaining buses, and returns the first matching function
pub fn find_device(vendor_id: u16, device_id: u16) -> Option<PciDeviceInfo> {
    for bus in 0..BUS_COUNT {
        for device in 0..DEVICES_PER_BUS {
            let function_zero = PciLocation::new(bus as u8, device, 0);
            let (vendor_function_zero, _) = read_vendor_and_device(function_zero);
            if vendor_function_zero == INVALID_VENDOR_ID {
                continue;
            }

            let header_type = config_read_dword(function_zero, OFFSET_HEADER_TYPE);
            let is_multifunction =
                (header_type >> HEADER_TYPE_SHIFT) & HEADER_TYPE_MULTIFUNCTION_BIT != 0;
            let function_count = if is_multifunction {
                FUNCTIONS_PER_DEVICE
            } else {
                1
            };

            for function in 0..function_count {
                let location = PciLocation::new(bus as u8, device, function);
                let (found_vendor, found_device) = read_vendor_and_device(location);
                if found_vendor == vendor_id && found_device == device_id {
                    return Some(PciDeviceInfo {
                        location,
                        vendor_id: found_vendor,
                        device_id: found_device,
                    });
                }
            }
        }
    }

    None
}

fn decode_bar(location: PciLocation, bar_index: u8) -> PciBar {
    debug_assert!(bar_index < BAR_COUNT);

    let bar_offset = OFFSET_BAR0 + bar_index * BAR_STRIDE_BYTES;
    let command_original = config_read_dword(location, OFFSET_COMMAND_STATUS);
    let decode_mask = COMMAND_IO_SPACE | COMMAND_MEMORY_SPACE;
    config_write_dword(
        location,
        OFFSET_COMMAND_STATUS,
        command_original & COMMAND_MASK & !decode_mask,
    );

    let bar_original = config_read_dword(location, bar_offset);
    config_write_dword(location, bar_offset, BAR_SIZING_PATTERN);
    let size_mask_low = config_read_dword(location, bar_offset);
    config_write_dword(location, bar_offset, bar_original);

    let is_io = bar_original & BAR_IO_SPACE_BIT != 0;
    let is_64_bit = !is_io && bar_original & BAR_MEMORY_TYPE_MASK == BAR_MEMORY_TYPE_64_BIT;

    let mut address_high = 0u32;
    let mut size_mask_high = 0u32;
    if is_64_bit {
        debug_assert!(bar_index + 1 < BAR_COUNT);

        let high_offset = bar_offset + BAR_STRIDE_BYTES;
        address_high = config_read_dword(location, high_offset);
        config_write_dword(location, high_offset, BAR_SIZING_PATTERN);
        size_mask_high = config_read_dword(location, high_offset);
        config_write_dword(location, high_offset, address_high);
    }

    config_write_dword(
        location,
        OFFSET_COMMAND_STATUS,
        command_original & COMMAND_MASK,
    );

    let address_mask_low = if is_io {
        BAR_IO_ADDRESS_MASK
    } else {
        BAR_MEMORY_ADDRESS_MASK
    };
    let masked_size_low = size_mask_low & address_mask_low;
    if masked_size_low == 0 && size_mask_high == 0 {
        return PciBar::Unused;
    }

    if is_io {
        return PciBar::Io {
            port_base: bar_original & BAR_IO_ADDRESS_MASK,
            size_bytes: (!masked_size_low).wrapping_add(1),
        };
    }

    let size_bytes = if is_64_bit {
        let size_mask = ((size_mask_high as u64) << 32) | masked_size_low as u64;
        (!size_mask).wrapping_add(1)
    } else {
        (!masked_size_low).wrapping_add(1) as u64
    };

    PciBar::Memory {
        address: ((address_high as u64) << 32) | (bar_original & BAR_MEMORY_ADDRESS_MASK) as u64,
        size_bytes,
        is_64_bit,
        is_prefetchable: bar_original & BAR_PREFETCHABLE_BIT != 0,
    }
}

pub fn read_bars(location: PciLocation) -> [PciBar; BAR_COUNT as usize] {
    let mut bars = [PciBar::Unused; BAR_COUNT as usize];

    let mut bar_index = 0;
    while bar_index < BAR_COUNT {
        let bar = decode_bar(location, bar_index);
        bars[bar_index as usize] = bar;
        bar_index += if bar.takes_two_slots() { 2 } else { 1 };
    }

    bars
}

pub fn enable_memory_and_bus_master(location: PciLocation) {
    let command_status = config_read_dword(location, OFFSET_COMMAND_STATUS);
    let command = (command_status & COMMAND_MASK) | COMMAND_MEMORY_SPACE | COMMAND_BUS_MASTER;

    config_write_dword(location, OFFSET_COMMAND_STATUS, command);
}
