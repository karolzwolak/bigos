#![no_std]
#![no_main]
#![feature(portable_simd)]

extern crate alloc;
extern crate rustspace;

use core::arch::global_asm;
use rustspace::{
    EventReader, WindowInfo,
    gfx::surface::UserSurface,
    sys_get_window_info,
    terminal::{Terminal, scrollback::Scrollback},
};
use x86_64::structures::paging::PageTableFlags;

global_asm!(
    ".section .text.entry",
    ".global _start",
    "_start:",
    "    call main",
    "    ud2",
);

#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    let window_id = unsafe { rustspace::sys_create_window(600, 400, 50, 50) };
    if window_id == u32::MAX {
        rustspace::println!("theophe: create_window failed");
        unsafe { rustspace::sys_exit(1) }
    }
    rustspace::println!("theophe: window id={}", window_id);

    let pixels = unsafe { rustspace::sys_map_window_buffer(window_id) };
    if pixels.is_null() {
        rustspace::println!("theophe: map_window_buffer failed");
        unsafe { rustspace::sys_exit(1) }
    }
    let mut window_info = WindowInfo::zeroed();
    if !unsafe { sys_get_window_info(window_id, &mut window_info) } {
        rustspace::println!("theophe: get_window_info failed");
        unsafe { rustspace::sys_exit(1) }
    }
    let pixels_second = ((pixels as u64) + window_info.front_buffer_offset) as *mut u32;

    let (width, height) = unsafe { rustspace::sys_get_window_size(window_id) };
    if width == 0 || height == 0 {
        rustspace::println!("theophe: get_window_size failed");
        unsafe { rustspace::sys_exit(1) }
    }

    rustspace::println!(
        "theophe: window {}x{} id={} mapped at {:p}, front: {:p}",
        width,
        height,
        window_id,
        pixels,
        pixels_second
    );

    unsafe { rustspace::syscall1(rustspace::SYS_FOCUS_WINDOW, window_id as u64) };

    let scrollback = {
        let page_flags =
            PageTableFlags::WRITABLE | PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        let mut buffer_info = rustspace::CircularBufferInfo::zeroed();
        let ok = unsafe {
            rustspace::sys_create_circular_buffer(64 * 1024, page_flags, &mut buffer_info)
        };
        if !ok {
            rustspace::println!("theophe: cant create circular buffer");
            unsafe { rustspace::sys_exit(1) }
        }
        rustspace::println!(
            "theophe: circular buffer base={:#x} view_size={} total={}",
            buffer_info.virtual_base,
            buffer_info.view_size,
            buffer_info.total_virtual_size
        );
        Scrollback::new(buffer_info)
    };

    let surface = unsafe { UserSurface::new(pixels, pixels_second, width, height) };
    let mut theophe = Terminal::new(surface, window_id, scrollback);

    rustspace::println!("theophe: writing");

    theophe.write_line("Theophe");
    theophe.write_line("=======================");

    rustspace::println!("theophe: starting loop");

    rustspace::init_programs();

    rustspace::println!(
        "theophe: window info: {}x{} event_buffer_vaddr: {}",
        window_info.width,
        window_info.height,
        window_info.event_buffer_vaddr,
    );

    let mut event_reader = unsafe { EventReader::new(window_info.event_buffer_vaddr as usize) };

    unsafe { rustspace::sys_yield() };

    let mut frame: u32 = 0;
    loop {
        let mut handled_event = false;
        loop {
            let event = event_reader.try_read(window_id);
            match event {
                Some(event) => {
                    handled_event = true;
                    theophe.handle_event(event);
                }
                None => break,
            }
        }

        let backbuffer_redraw_required = theophe.needs_redraw;
        if theophe.needs_redraw {
            theophe.render();
            theophe.needs_redraw = false;
        }
        unsafe { rustspace::sys_present_window(window_id) };
        unsafe {
            theophe.draw_target.swap();
            theophe.needs_redraw = backbuffer_redraw_required;
        }

        frame = frame.wrapping_add(1);
    }

    // unsafe { rustspace::sys_exit(0) };
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    rustspace::println!("theophe: panic");
    unsafe { rustspace::sys_exit(1) }
}
