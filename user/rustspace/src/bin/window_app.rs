#![no_std]
#![no_main]
#![feature(portable_simd)]

extern crate alloc;
extern crate rustspace;

use alloc::format;
use core::arch::global_asm;
use rustspace::gfx::{color::Rgba8888UNORM, surface::UserSurface};

const FRAME_INTERVAL_MS: u64 = 16;

global_asm!(
    ".section .text.entry",
    ".global _start",
    "_start:",
    "    call rust_main",
    "    ud2",
);

#[unsafe(no_mangle)]
pub extern "C" fn rust_main() -> ! {
    let window_id = unsafe { rustspace::sys_create_window(320, 240, 100, 100) };
    if window_id == u32::MAX {
        rustspace::println!("game: create_window failed");
        unsafe { rustspace::sys_exit(1) }
    }

    let pixels = unsafe { rustspace::sys_map_window_buffer(window_id) };
    if pixels.is_null() {
        rustspace::println!("game: map_window_buffer failed");
        unsafe { rustspace::sys_exit(1) }
    }
    let mut window_info = rustspace::WindowInfo::zeroed();
    if !unsafe { rustspace::sys_get_window_info(window_id, &mut window_info) } {
        rustspace::println!("game: get_window_info failed");
        unsafe { rustspace::sys_exit(1) }
    }
    let pixels_second = ((pixels as u64) + window_info.front_buffer_offset) as *mut u32;

    let (width, height) = unsafe { rustspace::sys_get_window_size(window_id) };
    rustspace::println!(
        "game: window {}x{} id={} mapped at {:p}",
        width,
        height,
        window_id,
        pixels
    );

    let mut surface = unsafe { UserSurface::new(pixels, pixels_second, width, height) };

    let mut frame: u32 = 0;
    loop {
        // Animate a simple color gradient that shifts every frame
        let r_base = (frame & 0xFF) as u8;
        let color = Rgba8888UNORM::from_rgb(r_base, 0x40, 0x80);
        surface.clear(color);

        // Draw a moving horizontal bar
        let bar_y = (frame / 2 % height) as u32;
        for x in 0..width {
            surface.write_pixel(x, bar_y, Rgba8888UNORM::WHITE);
            if bar_y + 1 < height {
                surface.write_pixel(x, bar_y + 1, Rgba8888UNORM::WHITE);
            }
        }

        unsafe { rustspace::sys_present_window(window_id) };
        unsafe { rustspace::sys_sleep_ms(FRAME_INTERVAL_MS) };

        frame = frame.wrapping_add(1);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    rustspace::println!("game: panic");
    unsafe { rustspace::sys_exit(1) }
}
