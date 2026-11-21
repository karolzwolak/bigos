use core::cmp::min;
use core::ffi::{CStr, c_char, c_int, c_long, c_void};
use core::ptr::{copy_nonoverlapping, null_mut};
use core::sync::atomic::{AtomicUsize, Ordering};

use rustspace::{
    FD_FLAG_READ, StatFlat, sys_close_file, sys_open_file, sys_read_file_at, sys_stat_file,
    sys_write,
};

use super::memory::{free, malloc};

pub const ERRNO_NOT_FOUND: c_int = 2;
pub const ERRNO_READ_ONLY_FILESYSTEM: c_int = 30;
pub const ERRNO_INVALID: c_int = 22;
pub const ERRNO_RANGE: c_int = 34;

pub const STDERR_FD: u64 = 2;

const SEEK_SET: c_int = 0;
const SEEK_CUR: c_int = 1;
const SEEK_END: c_int = 2;
const C_FAILURE: c_int = -1;
const C_SUCCESS: c_int = 0;
const MAX_STREAMS: usize = 8;
const STDOUT_SLOT: usize = 0;
const STDERR_SLOT: usize = 1;

/// Files up to this size are read whole into memory at fopen
pub const PRELOAD_MAX_BYTES: usize = 8 * 1024 * 1024;
static PRELOAD_LIMIT_BYTES: AtomicUsize = AtomicUsize::new(PRELOAD_MAX_BYTES);

#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static mut errno: c_int = 0;

pub fn set_errno(code: c_int) {
    // SAFETY: the doom process is single threaded
    unsafe { errno = code };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StreamKind {
    Closed,
    Stdout,
    Stderr,
    File,
    Preloaded,
}

pub struct Stream {
    pub kind: StreamKind,
    pub fd: usize,
    pub position_bytes: usize,
    pub size_bytes: usize,
    pub preload: *mut u8,
}

impl Stream {
    pub const CLOSED: Self = Self {
        kind: StreamKind::Closed,
        fd: 0,
        position_bytes: 0,
        size_bytes: 0,
        preload: null_mut(),
    };

    const fn with_kind(kind: StreamKind) -> Self {
        Self {
            kind,
            ..Self::CLOSED
        }
    }

    /// Reads at the position and advances it, returns the bytes read
    unsafe fn read_bytes(&mut self, destination: *mut u8, requested_bytes: usize) -> usize {
        debug_assert!(matches!(
            self.kind,
            StreamKind::File | StreamKind::Preloaded
        ));

        let available_bytes = self.size_bytes.saturating_sub(self.position_bytes);
        let read_bytes = min(requested_bytes, available_bytes);
        if read_bytes == 0 {
            return 0;
        }

        let read_bytes = match self.kind {
            StreamKind::Preloaded => {
                unsafe {
                    copy_nonoverlapping(
                        self.preload.add(self.position_bytes),
                        destination,
                        read_bytes,
                    )
                };

                read_bytes
            }
            _ => {
                // SAFETY: the caller owns destination for requested_bytes
                let buffer = unsafe { core::slice::from_raw_parts_mut(destination, read_bytes) };

                sys_read_file_at(self.fd, self.position_bytes, buffer).unwrap_or(0)
            }
        };
        self.position_bytes += read_bytes;

        read_bytes
    }
}

static mut STREAMS: [Stream; MAX_STREAMS] = {
    let mut streams = [const { Stream::CLOSED }; MAX_STREAMS];
    streams[STDOUT_SLOT] = Stream::with_kind(StreamKind::Stdout);
    streams[STDERR_SLOT] = Stream::with_kind(StreamKind::Stderr);

    streams
};

#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static mut stdout: *mut Stream = unsafe { &raw mut STREAMS[STDOUT_SLOT] };

#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static mut stderr: *mut Stream = unsafe { &raw mut STREAMS[STDERR_SLOT] };

/// Lets a test make a small file take the non preloaded path
#[unsafe(no_mangle)]
pub extern "C" fn bigc_set_preload_max_bytes(limit_bytes: usize) {
    PRELOAD_LIMIT_BYTES.store(limit_bytes, Ordering::Relaxed);
}

fn find_free_stream() -> *mut Stream {
    let first_stream = &raw mut STREAMS as *mut Stream;

    for slot in 0..MAX_STREAMS {
        // SAFETY: slot is in range and the doom process is single threaded
        let stream = unsafe { first_stream.add(slot) };
        if unsafe { (*stream).kind } == StreamKind::Closed {
            return stream;
        }
    }

    null_mut()
}

/// Reads a whole file into a malloc block, null when the memory or the read fails
fn preload_file(fd: usize, size_bytes: usize) -> *mut u8 {
    let buffer = unsafe { malloc(size_bytes.max(1)) } as *mut u8;
    if buffer.is_null() {
        return null_mut();
    }

    let mut filled_bytes = 0;
    while filled_bytes < size_bytes {
        // SAFETY: the block holds size_bytes and filled_bytes < size_bytes
        let remaining = unsafe {
            core::slice::from_raw_parts_mut(buffer.add(filled_bytes), size_bytes - filled_bytes)
        };
        match sys_read_file_at(fd, filled_bytes, remaining) {
            Ok(read_bytes) if read_bytes > 0 => filled_bytes += read_bytes,
            _ => {
                unsafe { free(buffer as *mut c_void) };

                return null_mut();
            }
        }
    }

    buffer
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fopen(path: *const c_char, mode: *const c_char) -> *mut Stream {
    // SAFETY: the caller passes NUL terminated strings
    let (path_bytes, mode_bytes) = unsafe {
        (
            CStr::from_ptr(path).to_bytes(),
            CStr::from_ptr(mode).to_bytes(),
        )
    };
    debug_assert!(path_bytes.is_ascii());

    let read_only = mode_bytes.first() == Some(&b'r') && !mode_bytes.contains(&b'+');
    if !read_only {
        set_errno(ERRNO_READ_ONLY_FILESYSTEM);

        return null_mut();
    }

    // SAFETY: checked ASCII before this
    let path_str = unsafe { core::str::from_utf8_unchecked(path_bytes) };
    let mut stat = StatFlat::zeroed();
    if sys_stat_file(path_str, &mut stat).is_err() {
        set_errno(ERRNO_NOT_FOUND);

        return null_mut();
    }

    let stream = find_free_stream();
    if stream.is_null() {
        set_errno(ERRNO_INVALID);

        return null_mut();
    }

    let Ok(fd) = sys_open_file(path_str, FD_FLAG_READ) else {
        set_errno(ERRNO_NOT_FOUND);

        return null_mut();
    };

    let size_bytes = stat.size as usize;
    let mut new_stream = Stream {
        kind: StreamKind::File,
        fd,
        position_bytes: 0,
        size_bytes,
        preload: null_mut(),
    };
    if size_bytes <= PRELOAD_LIMIT_BYTES.load(Ordering::Relaxed) {
        let preload = preload_file(fd, size_bytes);
        if !preload.is_null() {
            let _ = sys_close_file(fd);
            new_stream.kind = StreamKind::Preloaded;
            new_stream.preload = preload;
        }
    }

    unsafe { stream.write(new_stream) };

    stream
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fclose(stream: *mut Stream) -> c_int {
    let stream = unsafe { &mut *stream };
    debug_assert!(matches!(
        stream.kind,
        StreamKind::File | StreamKind::Preloaded
    ));

    match stream.kind {
        StreamKind::Preloaded => unsafe { free(stream.preload as *mut c_void) },
        _ => {
            let _ = sys_close_file(stream.fd);
        }
    }
    *stream = Stream::CLOSED;

    C_SUCCESS
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fread(
    buffer: *mut c_void,
    item_size_bytes: usize,
    item_count: usize,
    stream: *mut Stream,
) -> usize {
    let stream = unsafe { &mut *stream };
    debug_assert!(matches!(
        stream.kind,
        StreamKind::File | StreamKind::Preloaded
    ));

    let requested_bytes = item_size_bytes * item_count;
    if requested_bytes == 0 {
        return 0;
    }

    let read_bytes = unsafe { stream.read_bytes(buffer as *mut u8, requested_bytes) };

    read_bytes / item_size_bytes
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fseek(stream: *mut Stream, offset_bytes: c_long, origin: c_int) -> c_int {
    let stream = unsafe { &mut *stream };
    if !matches!(stream.kind, StreamKind::File | StreamKind::Preloaded) {
        set_errno(ERRNO_INVALID);

        return C_FAILURE;
    }

    let base_bytes: i64 = match origin {
        SEEK_SET => 0,
        SEEK_CUR => stream.position_bytes as i64,
        SEEK_END => stream.size_bytes as i64,
        _ => {
            set_errno(ERRNO_INVALID);

            return C_FAILURE;
        }
    };
    let target_bytes = base_bytes + offset_bytes;
    if target_bytes < 0 {
        set_errno(ERRNO_INVALID);

        return C_FAILURE;
    }

    stream.position_bytes = target_bytes as usize;

    C_SUCCESS
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ftell(stream: *mut Stream) -> c_long {
    unsafe { (*stream).position_bytes as c_long }
}

#[unsafe(no_mangle)]
pub extern "C" fn fflush(_stream: *mut Stream) -> c_int {
    C_SUCCESS
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bigc_write(stream: *mut Stream, bytes: *const u8, length: usize) -> usize {
    let stream = unsafe { &*stream };
    // SAFETY: the caller passes a live buffer of length bytes
    let slice = unsafe { core::slice::from_raw_parts(bytes, length) };

    match stream.kind {
        StreamKind::Stdout => {
            rustspace::channel::write_stdout(slice);

            length
        }
        StreamKind::Stderr => {
            unsafe { sys_write(STDERR_FD, bytes, length) };

            length
        }
        // files are read only, so a write never reaches one
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fwrite(
    buffer: *const c_void,
    item_size_bytes: usize,
    item_count: usize,
    stream: *mut Stream,
) -> usize {
    if item_size_bytes == 0 {
        return 0;
    }

    let written_bytes =
        unsafe { bigc_write(stream, buffer as *const u8, item_size_bytes * item_count) };

    written_bytes / item_size_bytes
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fputc(character: c_int, stream: *mut Stream) -> c_int {
    let byte = character as u8;
    if unsafe { bigc_write(stream, &byte, 1) } == 1 {
        byte as c_int
    } else {
        C_FAILURE
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn putc(character: c_int, stream: *mut Stream) -> c_int {
    unsafe { fputc(character, stream) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn putchar(character: c_int) -> c_int {
    unsafe { fputc(character, stdout) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn puts(text: *const c_char) -> c_int {
    // SAFETY: the caller passes a NUL terminated string
    let bytes = unsafe { CStr::from_ptr(text).to_bytes() };

    unsafe {
        bigc_write(stdout, bytes.as_ptr(), bytes.len());
        fputc(b'\n' as c_int, stdout);
    }

    C_SUCCESS
}
