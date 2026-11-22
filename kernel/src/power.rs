use crate::memory::paging::IdentityAcpiHandler;
use crate::serial_println;
use acpi::address::{AddressSpace, GenericAddress};
use acpi::sdt::fadt::Fadt;
use acpi::{AcpiTables, Handler};
use spin::Once;
use x86_64::instructions::port::Port;

// ACPI spec 4.8.3 and 16
const PM1_CONTROL_SLP_EN: u16 = 1 << 13;
const PM1_CONTROL_SLP_TYP_SHIFT: u16 = 10;
const SLP_TYP_MASK: u8 = 0b111;
const SDT_HEADER_SIZE_BYTES: usize = 36;
const AML_S5_NAME: &[u8; 4] = b"_S5_";
const AML_NAME_OP: u8 = 0x08;
const AML_ROOT_PREFIX: u8 = b'\\';
const AML_PACKAGE_OP: u8 = 0x12;
const AML_BYTE_PREFIX: u8 = 0x0A;
const AML_ZERO_OP: u8 = 0x00;
const AML_ONE_OP: u8 = 0x01;
const AML_PKG_LENGTH_EXTRA_BYTES_SHIFT: u8 = 6;
const AML_S5_PACKAGE_MIN_BYTES: usize = 5;
const POWER_OFF_GRACE_US: u64 = 500_000;

#[derive(Debug, Clone, Copy)]
pub struct SleepControl {
    pub pm1a_control_port: u16,
    pub pm1b_control_port: u16,
    pub slp_typ_a: u8,
    pub slp_typ_b: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct PowerInfo {
    /// None if no FADT PM1a block or no _S5 object in the DSDT
    pub sleep_control: Option<SleepControl>,
}

static POWER_INFO: Once<PowerInfo> = Once::new();

pub fn power_info() -> PowerInfo {
    POWER_INFO.get().copied().unwrap_or(PowerInfo {
        sleep_control: None,
    })
}

fn io_port(address: &GenericAddress) -> Option<u16> {
    match address.address_space {
        AddressSpace::SystemIo if address.address != 0 => u16::try_from(address.address).ok(),
        _ => None,
    }
}

/// Reads one element of the _S5 package, returns the value and the bytes it took
fn parse_aml_byte_data(aml: &[u8]) -> Option<(u8, usize)> {
    match *aml.first()? {
        AML_BYTE_PREFIX => Some((*aml.get(1)?, 2)),
        AML_ZERO_OP => Some((0, 1)),
        AML_ONE_OP => Some((1, 1)),
        _ => None,
    }
}

pub fn parse_s5_sleep_types(aml: &[u8]) -> Option<(u8, u8)> {
    let name_index = aml
        .windows(AML_S5_NAME.len())
        .position(|window| window == AML_S5_NAME)?;

    let is_named_object = match name_index {
        0 => false,
        1 => aml[0] == AML_NAME_OP,
        _ => {
            aml[name_index - 1] == AML_NAME_OP
                || (aml[name_index - 1] == AML_ROOT_PREFIX && aml[name_index - 2] == AML_NAME_OP)
        }
    };
    let package = &aml[name_index + AML_S5_NAME.len()..];
    if !is_named_object || package.len() < AML_S5_PACKAGE_MIN_BYTES || package[0] != AML_PACKAGE_OP
    {
        return None;
    }

    let pkg_length_bytes = 1 + (package[1] >> AML_PKG_LENGTH_EXTRA_BYTES_SHIFT) as usize;
    let elements = package.get(1 + pkg_length_bytes + 1..)?;

    let (slp_typ_a, used_bytes) = parse_aml_byte_data(elements)?;
    let (slp_typ_b, _) = parse_aml_byte_data(&elements[used_bytes..])?;

    Some((slp_typ_a & SLP_TYP_MASK, slp_typ_b & SLP_TYP_MASK))
}

fn read_sleep_control(
    fadt: &Fadt,
    tables: &AcpiTables<IdentityAcpiHandler>,
    handler: &IdentityAcpiHandler,
) -> Option<SleepControl> {
    let pm1a_control_port = io_port(&fadt.pm1a_control_block().ok()?)?;
    let pm1b_control_port = fadt
        .pm1b_control_block()
        .ok()
        .flatten()
        .and_then(|block| io_port(&block))
        .unwrap_or(0);

    let dsdt = tables.dsdt().ok()?;
    let length = dsdt.length as usize;
    if length <= SDT_HEADER_SIZE_BYTES {
        return None;
    }

    // SAFETY: the DSDT is ACPI reclaimable memory, mapped through the HHDM like every table
    let mapping = unsafe { handler.map_physical_region::<u8>(dsdt.phys_address, length) };
    let table = unsafe { core::slice::from_raw_parts(mapping.virtual_start.as_ptr(), length) };
    let (slp_typ_a, slp_typ_b) = parse_s5_sleep_types(&table[SDT_HEADER_SIZE_BYTES..])?;

    Some(SleepControl {
        pm1a_control_port,
        pm1b_control_port,
        slp_typ_a,
        slp_typ_b,
    })
}

pub fn init_from_acpi(tables: &AcpiTables<IdentityAcpiHandler>, handler: &IdentityAcpiHandler) {
    let info = match tables.find_table::<Fadt>() {
        Some(fadt) => PowerInfo {
            sleep_control: read_sleep_control(&fadt, tables, handler),
        },
        None => PowerInfo {
            sleep_control: None,
        },
    };

    serial_println!("power: S5 {:?}", info.sleep_control);
    POWER_INFO.call_once(|| info);
}

fn spin_for_us(duration_us: u64) {
    let start_us = crate::interrupts::tsc_timestamp_us();
    while crate::interrupts::tsc_timestamp_us() - start_us < duration_us {
        core::hint::spin_loop();
    }
}

pub fn enter_s5() -> bool {
    serial_println!("power: enter_s5");

    let Some(sleep_control) = power_info().sleep_control else {
        return false;
    };

    let control_a =
        ((sleep_control.slp_typ_a as u16) << PM1_CONTROL_SLP_TYP_SHIFT) | PM1_CONTROL_SLP_EN;
    let control_b =
        ((sleep_control.slp_typ_b as u16) << PM1_CONTROL_SLP_TYP_SHIFT) | PM1_CONTROL_SLP_EN;

    serial_println!("power: entering s5");

    // SAFETY: the ports come from the FADT, writing SLP_EN is the documented way to sleep
    unsafe {
        if sleep_control.pm1b_control_port != 0 {
            Port::<u16>::new(sleep_control.pm1b_control_port).write(control_b);
        }
        Port::<u16>::new(sleep_control.pm1a_control_port).write(control_a);
    }

    spin_for_us(POWER_OFF_GRACE_US);
    serial_println!("power: still running after S5 was requested");

    false
}
