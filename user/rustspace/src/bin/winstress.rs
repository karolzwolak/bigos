#![no_std]
#![no_main]

extern crate alloc;

use rustspace::{
    CircularBufferInfo, WindowInfo, sys_allocate, sys_create_circular_buffer, sys_create_window,
    sys_destroy_window, sys_exit, sys_get_window_info, sys_map_window_buffer, sys_write,
};
use x86_64::structures::paging::PageTableFlags;

const CIRCULAR_BUFFER_SIZE: usize = 64 * 1024;
const CIRCULAR_BUFFER_COUNT: usize = 2;
const HEAP_ALLOC_COUNT: usize = 100;
const HEAP_ALLOC_SIZE: usize = 16 * 1024;
const INVALID_ALLOC: u64 = u64::MAX;
const WINDOW_WIDTH: u32 = 100;
const WINDOW_HEIGHT: u32 = 100;
const WINDOW_X: i32 = 10;
const WINDOW_Y: i32 = 10;
const PIXEL_A: u32 = 0x1122_3344;
const PIXEL_B: u32 = 0x5566_7788;
const PIXEL_C: u32 = 0x99AA_BBCC;
const BYTE_A: u8 = 0xA5;
const BYTE_B: u8 = 0x5A;
const WRAP_PROBE_OFFSET: usize = 5;

const EXIT_CIRCULAR_CREATE: i32 = 10;
const EXIT_CIRCULAR_ALIAS: i32 = 11;
const EXIT_HEAP: i32 = 12;
const EXIT_WINDOW_CREATE: i32 = 20;
const EXIT_WINDOW_MAP: i32 = 21;
const EXIT_WINDOW_INFO: i32 = 22;
const EXIT_EVENT_BUFFER_SHARED: i32 = 23;
const EXIT_WINDOW_ID_NOT_REUSED: i32 = 24;
const EXIT_PIXEL_VERIFY: i32 = 25;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { sys_write(2, b"winstress panic\n".as_ptr(), 16) };
    unsafe { sys_exit(1) }
}

fn write_str(s: &[u8]) {
    unsafe { sys_write(1, s.as_ptr(), s.len()) };
}

fn fail(message: &[u8], code: i32) -> ! {
    write_str(message);
    unsafe { sys_exit(code) }
}

fn check_circular_buffer() {
    let page_flags =
        PageTableFlags::WRITABLE | PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    let mut info = CircularBufferInfo::zeroed();
    if !unsafe { sys_create_circular_buffer(CIRCULAR_BUFFER_SIZE, page_flags, &mut info) } {
        fail(
            b"winstress: circular buffer create failed\n",
            EXIT_CIRCULAR_CREATE,
        );
    }

    let base = info.virtual_base as *mut u8;
    let view_size = info.view_size as usize;
    unsafe {
        base.write_volatile(BYTE_A);
        if base.add(view_size).read_volatile() != BYTE_A {
            fail(
                b"winstress: second view does not alias\n",
                EXIT_CIRCULAR_ALIAS,
            );
        }

        base.add(view_size + WRAP_PROBE_OFFSET)
            .write_volatile(BYTE_B);
        if base.add(WRAP_PROBE_OFFSET).read_volatile() != BYTE_B {
            fail(
                b"winstress: first view does not alias\n",
                EXIT_CIRCULAR_ALIAS,
            );
        }
    }
}

fn create_and_map_window() -> (u32, *mut u32, WindowInfo) {
    let window_id = unsafe { sys_create_window(WINDOW_WIDTH, WINDOW_HEIGHT, WINDOW_X, WINDOW_Y) };
    if window_id == u32::MAX {
        fail(b"winstress: create_window failed\n", EXIT_WINDOW_CREATE);
    }

    let pixels = unsafe { sys_map_window_buffer(window_id) };
    if pixels.is_null() {
        fail(b"winstress: map_window_buffer failed\n", EXIT_WINDOW_MAP);
    }

    let mut info = WindowInfo::zeroed();
    if !unsafe { sys_get_window_info(window_id, &mut info) } || info.event_buffer_vaddr == 0 {
        fail(b"winstress: get_window_info failed\n", EXIT_WINDOW_INFO);
    }

    // touching the event buffer faults if it is not mapped
    unsafe { (info.event_buffer_vaddr as *const u32).read_volatile() };

    (window_id, pixels, info)
}

fn check_pixel(pixels: *mut u32, value: u32) {
    unsafe {
        pixels.write_volatile(value);
        if pixels.read_volatile() != value {
            fail(b"winstress: pixel verify failed\n", EXIT_PIXEL_VERIFY);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    write_str(b"winstress: start\n");

    for _ in 0..CIRCULAR_BUFFER_COUNT {
        check_circular_buffer();
    }
    write_str(b"winstress: circular buffers ok\n");

    for _ in 0..HEAP_ALLOC_COUNT {
        let ptr = unsafe { sys_allocate(HEAP_ALLOC_SIZE) };
        if ptr == INVALID_ALLOC || ptr == 0 {
            fail(b"winstress: alloc failed\n", EXIT_HEAP);
        }
    }
    write_str(b"winstress: heap ok\n");

    let (window_a, pixels_a, info_a) = create_and_map_window();
    check_pixel(pixels_a, PIXEL_A);

    let (_window_b, pixels_b, info_b) = create_and_map_window();
    check_pixel(pixels_b, PIXEL_B);
    if info_a.event_buffer_vaddr == info_b.event_buffer_vaddr {
        fail(
            b"winstress: windows share an event buffer\n",
            EXIT_EVENT_BUFFER_SHARED,
        );
    }
    write_str(b"winstress: two windows ok\n");

    // the destroyed window id is reused, its new frames must be the ones mapped
    unsafe { sys_destroy_window(window_a) };
    let (window_c, pixels_c, _info_c) = create_and_map_window();
    if window_c != window_a {
        fail(
            b"winstress: window id not reused\n",
            EXIT_WINDOW_ID_NOT_REUSED,
        );
    }
    check_pixel(pixels_c, PIXEL_C);
    write_str(b"winstress: window recreate ok\n");

    // exit with two windows still open, the kernel has to clean them up
    write_str(b"winstress: exiting with open windows\n");
    unsafe { sys_exit(0) }
}
