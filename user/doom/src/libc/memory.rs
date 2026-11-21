use alloc::alloc::{alloc, dealloc};
use core::alloc::Layout;
use core::cmp::min;
use core::ffi::c_void;
use core::ptr::{copy_nonoverlapping, null_mut, write_bytes};

const MALLOC_ALIGN_BYTES: usize = 16;
const BLOCK_HEADER_BYTES: usize = 16;

/// Reads the requested size stored in front of a user pointer
unsafe fn block_size_bytes(user_ptr: *mut u8) -> usize {
    debug_assert!((user_ptr as usize).is_multiple_of(MALLOC_ALIGN_BYTES));

    unsafe { (user_ptr.sub(BLOCK_HEADER_BYTES) as *const usize).read() }
}

fn block_layout(size_bytes: usize) -> Layout {
    debug_assert!(size_bytes <= isize::MAX as usize - BLOCK_HEADER_BYTES - MALLOC_ALIGN_BYTES);

    // SAFETY: the alignment is a power of two and the size was checked by the caller
    unsafe {
        Layout::from_size_align_unchecked(size_bytes + BLOCK_HEADER_BYTES, MALLOC_ALIGN_BYTES)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(size_bytes: usize) -> *mut c_void {
    if size_bytes > isize::MAX as usize / 2 {
        return null_mut();
    }

    let base = unsafe { alloc(block_layout(size_bytes)) };
    if base.is_null() {
        return null_mut();
    }
    debug_assert!((base as usize).is_multiple_of(MALLOC_ALIGN_BYTES));

    unsafe {
        (base as *mut usize).write(size_bytes);
        base.add(BLOCK_HEADER_BYTES) as *mut c_void
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn calloc(item_count: usize, item_size_bytes: usize) -> *mut c_void {
    let Some(total_bytes) = item_count.checked_mul(item_size_bytes) else {
        return null_mut();
    };

    let block = unsafe { malloc(total_bytes) };
    if !block.is_null() {
        unsafe { write_bytes(block as *mut u8, 0, total_bytes) };
    }

    block
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(block: *mut c_void) {
    if block.is_null() {
        return;
    }

    unsafe {
        let user_ptr = block as *mut u8;
        let size_bytes = block_size_bytes(user_ptr);
        dealloc(user_ptr.sub(BLOCK_HEADER_BYTES), block_layout(size_bytes));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(block: *mut c_void, new_size_bytes: usize) -> *mut c_void {
    if block.is_null() {
        return unsafe { malloc(new_size_bytes) };
    }

    if new_size_bytes == 0 {
        unsafe { free(block) };

        return null_mut();
    }

    let new_block = unsafe { malloc(new_size_bytes) };
    if new_block.is_null() {
        return null_mut();
    }

    unsafe {
        let old_size_bytes = block_size_bytes(block as *mut u8);
        copy_nonoverlapping(
            block as *const u8,
            new_block as *mut u8,
            min(old_size_bytes, new_size_bytes),
        );
        free(block);
    }

    new_block
}
