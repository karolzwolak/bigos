use crate::interrupts::tsc_timestamp_us;
use crate::io::rtc::{CENT_NO_CENTURY_REGISTER, read_rtc};
use crate::memory::paging::IdentityAcpiHandler;
use crate::serial_println;
use acpi::AcpiTables;
use acpi::sdt::fadt::Fadt;
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};

pub const SECONDS_PER_MINUTE: u32 = 60;
pub const SECONDS_PER_HOUR: u32 = 3600;
pub const SECONDS_PER_DAY: u32 = 86_400;
pub const UNIX_EPOCH_YEAR: u32 = 1970;
const MICROSECONDS_PER_SECOND: u64 = 1_000_000;
const UNIX_EPOCH_SHIFT_DAYS: u32 = 719468;
const DAYS_PER_ERA: u32 = 146097;
const YEARS_PER_ERA: u32 = 400;
const DAYS_PER_YEAR: u32 = 365;
const MONTHS_BEFORE_MARCH: u32 = 2;
const MARCH_BASED_MONTH_SHIFT: u32 = 3;
const MONTHS_PER_YEAR: u32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

// uses Howard Hinnant's date algorithms
impl DateTime {
    pub const UNIX_EPOCH: DateTime = DateTime {
        year: UNIX_EPOCH_YEAR,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    };

    pub const fn to_unix_seconds(&self) -> u32 {
        debug_assert!(self.year >= UNIX_EPOCH_YEAR);
        debug_assert!(self.month >= 1 && self.month <= MONTHS_PER_YEAR);
        debug_assert!(self.day >= 1 && self.day <= 31);
        debug_assert!(self.hour < 24 && self.minute < 60 && self.second < 60);

        let march_year = if self.month <= MONTHS_BEFORE_MARCH {
            self.year - 1
        } else {
            self.year
        };
        let era = march_year / YEARS_PER_ERA;
        let year_of_era = march_year - era * YEARS_PER_ERA;
        let march_month =
            (self.month + MONTHS_PER_YEAR - MARCH_BASED_MONTH_SHIFT) % MONTHS_PER_YEAR;
        let day_of_year = (153 * march_month + 2) / 5 + self.day - 1;
        let day_of_era =
            year_of_era * DAYS_PER_YEAR + year_of_era / 4 - year_of_era / 100 + day_of_year;
        let days = era * DAYS_PER_ERA + day_of_era - UNIX_EPOCH_SHIFT_DAYS;

        days * SECONDS_PER_DAY
            + self.hour * SECONDS_PER_HOUR
            + self.minute * SECONDS_PER_MINUTE
            + self.second
    }

    pub const fn from_unix_seconds(unix_seconds: u32) -> Self {
        let days = unix_seconds / SECONDS_PER_DAY + UNIX_EPOCH_SHIFT_DAYS;
        let second_of_day = unix_seconds % SECONDS_PER_DAY;

        let era = days / DAYS_PER_ERA;
        let day_of_era = days - era * DAYS_PER_ERA;
        let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524
            - day_of_era / 146_096)
            / DAYS_PER_YEAR;
        let day_of_year =
            day_of_era - (DAYS_PER_YEAR * year_of_era + year_of_era / 4 - year_of_era / 100);
        let march_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * march_month + 2) / 5 + 1;
        let month = (march_month + MARCH_BASED_MONTH_SHIFT - 1) % MONTHS_PER_YEAR + 1;
        let year = year_of_era + era * YEARS_PER_ERA + (month <= MONTHS_BEFORE_MARCH) as u32;

        DateTime {
            year,
            month,
            day,
            hour: second_of_day / SECONDS_PER_HOUR,
            minute: second_of_day % SECONDS_PER_HOUR / SECONDS_PER_MINUTE,
            second: second_of_day % SECONDS_PER_MINUTE,
        }
    }
}

/// CMOS index of the RTC century byte from the FADT
static RTC_CENTURY_REGISTER: AtomicU8 = AtomicU8::new(CENT_NO_CENTURY_REGISTER);
/// Unix seconds the RTC showed at boot
static BOOT_UNIX_SECONDS: AtomicU32 = AtomicU32::new(0);
/// tsc_timestamp_us() at the moment of the boot RTC read
static BOOT_RTC_READ_US: AtomicU64 = AtomicU64::new(0);

pub fn init_from_acpi(tables: &AcpiTables<IdentityAcpiHandler>) {
    let century_register = match tables.find_table::<Fadt>() {
        Some(fadt) => fadt.century,
        None => CENT_NO_CENTURY_REGISTER,
    };

    RTC_CENTURY_REGISTER.store(century_register, Ordering::Relaxed);
}

pub fn init_wall_clock() {
    let boot_time = read_rtc(RTC_CENTURY_REGISTER.load(Ordering::Relaxed));
    let read_us = tsc_timestamp_us();

    BOOT_UNIX_SECONDS.store(boot_time.to_unix_seconds(), Ordering::Relaxed);
    BOOT_RTC_READ_US.store(read_us, Ordering::Relaxed);

    serial_println!(
        "time: boot RTC {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        boot_time.year,
        boot_time.month,
        boot_time.day,
        boot_time.hour,
        boot_time.minute,
        boot_time.second
    );
}

pub fn boot_unix_seconds() -> u32 {
    BOOT_UNIX_SECONDS.load(Ordering::Relaxed)
}

#[inline]
pub fn now_unix_seconds() -> u32 {
    let elapsed_us = tsc_timestamp_us() - BOOT_RTC_READ_US.load(Ordering::Relaxed);

    BOOT_UNIX_SECONDS.load(Ordering::Relaxed) + (elapsed_us / MICROSECONDS_PER_SECOND) as u32
}
