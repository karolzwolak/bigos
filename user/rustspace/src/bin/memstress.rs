#![no_std]
#![no_main]

extern crate alloc;

use rustspace::{sys_allocate, sys_exit, sys_write};

const PROCESS_HEAP_SIZE_BYTES: usize = 2 * 1024 * 1024;
const ALLOC_COUNT: usize = 4;
const ALLOC_SIZE: usize = PROCESS_HEAP_SIZE_BYTES / ALLOC_COUNT + 4096;
const PATTERN: u8 = 0xAA;
const INVALID_ALLOC: u64 = u64::MAX;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { sys_write(2, b"memstress panic\n".as_ptr(), 16) };
    unsafe { sys_exit(1) }
}

fn write_str(s: &[u8]) {
    unsafe { sys_write(1, s.as_ptr(), s.len()) };
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    write_str(b"memstress: start\n");

    let mut ptrs: [u64; ALLOC_COUNT] = [0; ALLOC_COUNT];

    for i in 0..ALLOC_COUNT {
        let ptr = unsafe { sys_allocate(ALLOC_SIZE) };
        if ptr == INVALID_ALLOC || ptr == 0 {
            write_str(b"memstress: alloc failed\n");
            unsafe { sys_exit(2) }
        }
        ptrs[i] = ptr;
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, ALLOC_SIZE) };
        for b in slice.iter_mut() {
            *b = PATTERN.wrapping_add(i as u8);
        }
        for (off, b) in slice.iter().enumerate() {
            let expected = PATTERN.wrapping_add(i as u8);
            if *b != expected {
                write_str(b"memstress: verify failed\n");
                unsafe { sys_exit(3) }
            }
            let _ = off;
        }
        write_str(b"memstress: alloc ok\n");
    }

    let total = ALLOC_SIZE * ALLOC_COUNT;
    if total <= PROCESS_HEAP_SIZE_BYTES {
        write_str(b"memstress: total not > heap\n");
        unsafe { sys_exit(4) }
    }

    write_str(b"memstress: all allocs verified, exiting\n");
    unsafe { sys_exit(0) }
}
