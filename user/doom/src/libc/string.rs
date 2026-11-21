use core::cmp::Ordering;
use core::ffi::{c_char, c_int, c_void};
use core::ptr::{copy_nonoverlapping, null_mut, read_volatile, write_bytes};

use super::memory::malloc;

#[inline(always)]
fn fold_ascii_case(byte: u8) -> u8 {
    byte.to_ascii_lowercase()
}

fn ordering_to_c_int(ordering: Ordering) -> c_int {
    ordering as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memchr(block: *const c_void, value: c_int, length: usize) -> *mut c_void {
    let bytes = block as *const u8;
    let needle = value as u8;

    for index in 0..length {
        if unsafe { *bytes.add(index) } == needle {
            return unsafe { bytes.add(index) } as *mut c_void;
        }
    }

    null_mut()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strlen(text: *const c_char) -> usize {
    let mut length = 0;

    // volatile: the optimizer must not turn this loop back into a call to strlen
    while unsafe { read_volatile(text.add(length)) } != 0 {
        length += 1;
    }

    length
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcmp(left: *const c_char, right: *const c_char) -> c_int {
    let mut index = 0;

    loop {
        let (left_byte, right_byte) = unsafe { (*left.add(index) as u8, *right.add(index) as u8) };
        if left_byte != right_byte || left_byte == 0 {
            return ordering_to_c_int(left_byte.cmp(&right_byte));
        }
        index += 1;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strncmp(
    left: *const c_char,
    right: *const c_char,
    length: usize,
) -> c_int {
    for index in 0..length {
        let (left_byte, right_byte) = unsafe { (*left.add(index) as u8, *right.add(index) as u8) };
        if left_byte != right_byte || left_byte == 0 {
            return ordering_to_c_int(left_byte.cmp(&right_byte));
        }
    }

    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcasecmp(left: *const c_char, right: *const c_char) -> c_int {
    let mut index = 0;

    loop {
        let (left_byte, right_byte) = unsafe {
            (
                fold_ascii_case(*left.add(index) as u8),
                fold_ascii_case(*right.add(index) as u8),
            )
        };
        if left_byte != right_byte || left_byte == 0 {
            return ordering_to_c_int(left_byte.cmp(&right_byte));
        }
        index += 1;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strncasecmp(
    left: *const c_char,
    right: *const c_char,
    length: usize,
) -> c_int {
    for index in 0..length {
        let (left_byte, right_byte) = unsafe {
            (
                fold_ascii_case(*left.add(index) as u8),
                fold_ascii_case(*right.add(index) as u8),
            )
        };
        if left_byte != right_byte || left_byte == 0 {
            return ordering_to_c_int(left_byte.cmp(&right_byte));
        }
    }

    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strchr(text: *const c_char, value: c_int) -> *mut c_char {
    let needle = value as c_char;
    let mut cursor = text;

    loop {
        let current = unsafe { *cursor };
        if current == needle {
            return cursor as *mut c_char;
        }
        if current == 0 {
            return null_mut();
        }
        cursor = unsafe { cursor.add(1) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strrchr(text: *const c_char, value: c_int) -> *mut c_char {
    let needle = value as c_char;
    let mut cursor = text;
    let mut last_match: *mut c_char = null_mut();

    loop {
        let current = unsafe { *cursor };
        if current == needle {
            last_match = cursor as *mut c_char;
        }
        if current == 0 {
            return last_match;
        }
        cursor = unsafe { cursor.add(1) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strstr(text: *const c_char, needle: *const c_char) -> *mut c_char {
    let needle_length = unsafe { strlen(needle) };
    let mut cursor = text;

    loop {
        if unsafe { strncmp(cursor, needle, needle_length) } == 0 {
            return cursor as *mut c_char;
        }
        if unsafe { *cursor } == 0 {
            return null_mut();
        }
        cursor = unsafe { cursor.add(1) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strncpy(
    destination: *mut c_char,
    source: *const c_char,
    length: usize,
) -> *mut c_char {
    let mut copied = 0;

    while copied < length {
        let current = unsafe { *source.add(copied) };
        if current == 0 {
            break;
        }
        unsafe { *destination.add(copied) = current };
        copied += 1;
    }

    // pad the rest with zeros
    unsafe { write_bytes(destination.add(copied), 0, length - copied) };

    destination
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strdup(text: *const c_char) -> *mut c_char {
    let length_with_nul = unsafe { strlen(text) } + 1;
    let copy = unsafe { malloc(length_with_nul) } as *mut c_char;
    if !copy.is_null() {
        unsafe { copy_nonoverlapping(text, copy, length_with_nul) };
    }

    copy
}
