use core::ffi::{CStr, c_char, c_int};
use rustspace::{sys_exit, sys_write};

use super::stdio::{ERRNO_READ_ONLY_FILESYSTEM, STDERR_FD, set_errno};

const ASSERT_FAILED_EXIT_CODE: c_int = 134;
const C_FAILURE: c_int = -1;

#[unsafe(no_mangle)]
pub extern "C" fn exit(exit_code: c_int) -> ! {
    sys_exit(exit_code)
}

#[unsafe(no_mangle)]
pub extern "C" fn system(_command: *const c_char) -> c_int {
    C_FAILURE
}

#[unsafe(no_mangle)]
pub extern "C" fn mkdir(_path: *const c_char, _mode: u32) -> c_int {
    set_errno(ERRNO_READ_ONLY_FILESYSTEM);

    C_FAILURE
}

#[unsafe(no_mangle)]
pub extern "C" fn remove(_path: *const c_char) -> c_int {
    set_errno(ERRNO_READ_ONLY_FILESYSTEM);

    C_FAILURE
}

#[unsafe(no_mangle)]
pub extern "C" fn rename(_old_path: *const c_char, _new_path: *const c_char) -> c_int {
    set_errno(ERRNO_READ_ONLY_FILESYSTEM);

    C_FAILURE
}

fn write_stderr(bytes: &[u8]) {
    // SAFETY: the pointer and length come from a live slice
    unsafe { sys_write(STDERR_FD, bytes.as_ptr(), bytes.len()) };
}

fn write_stderr_number(number: c_int) {
    const MAX_DIGITS: usize = 11;

    let mut digits = [0u8; MAX_DIGITS];
    let mut start = MAX_DIGITS;
    let mut remaining = number.unsigned_abs();
    loop {
        start -= 1;
        digits[start] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    if number < 0 {
        write_stderr(b"-");
    }

    write_stderr(&digits[start..]);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bigc_assert_fail(
    expression: *const c_char,
    file: *const c_char,
    line: c_int,
) -> ! {
    // SAFETY: the assert macro passes string literals
    let (expression, file) = unsafe { (CStr::from_ptr(expression), CStr::from_ptr(file)) };

    write_stderr(b"assertion failed: ");
    write_stderr(expression.to_bytes());
    write_stderr(b" at ");
    write_stderr(file.to_bytes());
    write_stderr(b":");
    write_stderr_number(line);
    write_stderr(b"\n");

    sys_exit(ASSERT_FAILED_EXIT_CODE)
}
