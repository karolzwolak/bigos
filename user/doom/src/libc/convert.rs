use core::ffi::{c_char, c_int, c_long};

use super::stdio::{ERRNO_RANGE, set_errno};

const ASCII_CASE_BIT: u8 = 0x20;
const DECIMAL_BASE: u32 = 10;
const OCTAL_BASE: u32 = 8;
const HEX_BASE: u32 = 16;
const BASE_AUTODETECT: c_int = 0;
const MAX_ACCUMULATED_DIGITS: u32 = 19;
const MAX_POWER_OF_TEN_STEPS: i32 = 400;

fn is_space(byte: u8) -> bool {
    byte == b' ' || (b'\t'..=b'\r').contains(&byte)
}

fn digit_value(byte: u8) -> u32 {
    match byte {
        b'0'..=b'9' => (byte - b'0') as u32,
        b'a'..=b'z' => (byte - b'a') as u32 + DECIMAL_BASE,
        b'A'..=b'Z' => (byte - b'A') as u32 + DECIMAL_BASE,
        _ => u32::MAX,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtol(text: *const c_char, end: *mut *mut c_char, base: c_int) -> c_long {
    debug_assert!(base == BASE_AUTODETECT || (2..=36).contains(&base));

    let bytes = text as *const u8;
    let mut cursor = 0;
    while is_space(unsafe { *bytes.add(cursor) }) {
        cursor += 1;
    }

    let mut negative = false;
    match unsafe { *bytes.add(cursor) } {
        b'-' => {
            negative = true;
            cursor += 1;
        }
        b'+' => cursor += 1,
        _ => {}
    }

    let mut radix = base as u32;
    let has_hex_prefix = unsafe {
        *bytes.add(cursor) == b'0'
            && (*bytes.add(cursor + 1) | ASCII_CASE_BIT) == b'x'
            && digit_value(*bytes.add(cursor + 2)) < HEX_BASE
    };
    if (radix == 0 || radix == HEX_BASE) && has_hex_prefix {
        radix = HEX_BASE;
        cursor += 2;
    } else if radix == 0 {
        radix = if unsafe { *bytes.add(cursor) } == b'0' {
            OCTAL_BASE
        } else {
            DECIMAL_BASE
        };
    }

    let digits_start = cursor;
    let mut magnitude: u64 = 0;
    let mut overflowed = false;
    loop {
        let digit = digit_value(unsafe { *bytes.add(cursor) });
        if digit >= radix {
            break;
        }

        match magnitude
            .checked_mul(radix as u64)
            .and_then(|scaled| scaled.checked_add(digit as u64))
        {
            Some(next) => magnitude = next,
            None => overflowed = true,
        }
        cursor += 1;
    }

    if cursor == digits_start {
        if !end.is_null() {
            unsafe { *end = text as *mut c_char };
        }

        return 0;
    }
    if !end.is_null() {
        unsafe { *end = bytes.add(cursor) as *mut c_char };
    }

    let limit = if negative {
        c_long::MIN.unsigned_abs()
    } else {
        c_long::MAX as u64
    };
    if overflowed || magnitude > limit {
        set_errno(ERRNO_RANGE);

        return if negative { c_long::MIN } else { c_long::MAX };
    }

    if negative {
        (magnitude as c_long).wrapping_neg()
    } else {
        magnitude as c_long
    }
}

fn scale_by_power_of_ten(value: f64, exponent: i32) -> f64 {
    let steps = exponent.clamp(-MAX_POWER_OF_TEN_STEPS, MAX_POWER_OF_TEN_STEPS);
    let mut scaled = value;

    for _ in 0..steps.unsigned_abs() {
        if steps > 0 {
            scaled *= DECIMAL_BASE as f64;
        } else {
            scaled /= DECIMAL_BASE as f64;
        }
    }

    scaled
}

/// Decimal only
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtod(text: *const c_char, end: *mut *mut c_char) -> f64 {
    let bytes = text as *const u8;
    let mut cursor = 0;
    while is_space(unsafe { *bytes.add(cursor) }) {
        cursor += 1;
    }

    let mut negative = false;
    match unsafe { *bytes.add(cursor) } {
        b'-' => {
            negative = true;
            cursor += 1;
        }
        b'+' => cursor += 1,
        _ => {}
    }

    let mut mantissa: u64 = 0;
    let mut accumulated_digits = 0;
    let mut decimal_exponent: i32 = 0;
    let mut seen_digits = false;

    let mut seen_point = false;
    loop {
        let byte = unsafe { *bytes.add(cursor) };
        if byte.is_ascii_digit() {
            seen_digits = true;
            if accumulated_digits < MAX_ACCUMULATED_DIGITS {
                mantissa = mantissa * DECIMAL_BASE as u64 + (byte - b'0') as u64;
                if mantissa != 0 {
                    accumulated_digits += 1;
                }
                if seen_point {
                    decimal_exponent -= 1;
                }
            } else if !seen_point {
                decimal_exponent += 1;
            }
        } else if byte == b'.' && !seen_point {
            seen_point = true;
        } else {
            break;
        }
        cursor += 1;
    }

    if !seen_digits {
        if !end.is_null() {
            unsafe { *end = text as *mut c_char };
        }

        return 0.0;
    }

    if (unsafe { *bytes.add(cursor) } | ASCII_CASE_BIT) == b'e' {
        let mut exponent_cursor = cursor + 1;
        let mut exponent_negative = false;
        match unsafe { *bytes.add(exponent_cursor) } {
            b'-' => {
                exponent_negative = true;
                exponent_cursor += 1;
            }
            b'+' => exponent_cursor += 1,
            _ => {}
        }

        if unsafe { *bytes.add(exponent_cursor) }.is_ascii_digit() {
            let mut exponent: i32 = 0;
            while unsafe { *bytes.add(exponent_cursor) }.is_ascii_digit() {
                let digit = (unsafe { *bytes.add(exponent_cursor) } - b'0') as i32;
                exponent = exponent
                    .saturating_mul(DECIMAL_BASE as i32)
                    .saturating_add(digit);
                exponent_cursor += 1;
            }
            decimal_exponent = decimal_exponent.saturating_add(if exponent_negative {
                -exponent
            } else {
                exponent
            });
            cursor = exponent_cursor;
        }
    }

    if !end.is_null() {
        unsafe { *end = bytes.add(cursor) as *mut c_char };
    }

    let magnitude = scale_by_power_of_ten(mantissa as f64, decimal_exponent);

    if negative { -magnitude } else { magnitude }
}
