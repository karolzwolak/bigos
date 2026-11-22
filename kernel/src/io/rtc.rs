use crate::time::DateTime;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::instructions::port::Port;

const CMOS_INDEX_PORT: u16 = 0x70;
const CMOS_DATA_PORT: u16 = 0x71;

const RTC_REGISTER_SECONDS: u8 = 0x00;
const RTC_REGISTER_MINUTES: u8 = 0x02;
const RTC_REGISTER_HOURS: u8 = 0x04;
const RTC_REGISTER_DAY: u8 = 0x07;
const RTC_REGISTER_MONTH: u8 = 0x08;
const RTC_REGISTER_YEAR: u8 = 0x09;
const RTC_REGISTER_STATUS_A: u8 = 0x0A;
const RTC_REGISTER_STATUS_B: u8 = 0x0B;

const STATUS_A_UPDATE_IN_PROGRESS: u8 = 0x80;
const STATUS_B_24_HOUR: u8 = 0x02;
const STATUS_B_BINARY: u8 = 0x04;
const HOURS_PM_FLAG: u8 = 0x80;
const HOURS_PER_HALF_DAY: u32 = 12;
const YEARS_PER_CENTURY: u32 = 100;
const BCD_DIGIT_MASK: u8 = 0x0F;
const BCD_DIGIT_BITS: u8 = 4;
pub const CENT_NO_CENTURY_REGISTER: u8 = 0;
pub const DEFAULT_CENTURY: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtcRegisters {
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub day: u8,
    pub month: u8,
    pub year: u8,
    pub century: u8,
    pub status_b: u8,
}

impl RtcRegisters {
    pub fn decode(&self, has_century_register: bool) -> DateTime {
        let is_binary = self.status_b & STATUS_B_BINARY != 0;
        let is_24_hour = self.status_b & STATUS_B_24_HOUR != 0;
        let to_binary = |value: u8| -> u32 {
            if is_binary {
                value as u32
            } else {
                bcd_to_binary(value) as u32
            }
        };

        let mut hour = to_binary(self.hours & !HOURS_PM_FLAG);
        if !is_24_hour {
            // 12 AM is hour 0, 12 PM is hour 12
            hour %= HOURS_PER_HALF_DAY;
            if self.hours & HOURS_PM_FLAG != 0 {
                hour += HOURS_PER_HALF_DAY;
            }
        }

        let century = if has_century_register {
            to_binary(self.century)
        } else {
            DEFAULT_CENTURY
        };

        DateTime {
            year: century * YEARS_PER_CENTURY + to_binary(self.year),
            month: to_binary(self.month),
            day: to_binary(self.day),
            hour,
            minute: to_binary(self.minutes),
            second: to_binary(self.seconds),
        }
    }
}

pub const fn bcd_to_binary(value: u8) -> u8 {
    (value >> BCD_DIGIT_BITS) * 10 + (value & BCD_DIGIT_MASK)
}

fn read_cmos(index: u8) -> u8 {
    let mut index_port: Port<u8> = Port::new(CMOS_INDEX_PORT);
    let mut data_port: Port<u8> = Port::new(CMOS_DATA_PORT);

    // SAFETY: using CMOS ports
    unsafe {
        index_port.write(index);
        data_port.read()
    }
}

fn wait_while_update_in_progress() {
    while read_cmos(RTC_REGISTER_STATUS_A) & STATUS_A_UPDATE_IN_PROGRESS != 0 {
        core::hint::spin_loop();
    }
}

fn read_registers(century_register: u8) -> RtcRegisters {
    wait_while_update_in_progress();

    RtcRegisters {
        seconds: read_cmos(RTC_REGISTER_SECONDS),
        minutes: read_cmos(RTC_REGISTER_MINUTES),
        hours: read_cmos(RTC_REGISTER_HOURS),
        day: read_cmos(RTC_REGISTER_DAY),
        month: read_cmos(RTC_REGISTER_MONTH),
        year: read_cmos(RTC_REGISTER_YEAR),
        century: if century_register != CENT_NO_CENTURY_REGISTER {
            read_cmos(century_register)
        } else {
            0
        },
        status_b: read_cmos(RTC_REGISTER_STATUS_B),
    }
}

pub fn read_rtc(century_register: u8) -> DateTime {
    without_interrupts(|| {
        let mut previous = read_registers(century_register);
        loop {
            let current = read_registers(century_register);
            if current == previous {
                return current.decode(century_register != CENT_NO_CENTURY_REGISTER);
            }

            previous = current;
        }
    })
}
