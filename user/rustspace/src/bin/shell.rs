#![no_std]
#![no_main]

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

const WINDOW_BUFFER_COUNT: u8 = 2;
const JOB_POLL_INTERVAL_MS: u64 = 10;

global_asm!(
    ".section .text.entry",
    ".global _start",
    "_start:",
    "    call main",
    "    ud2",
);

#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    let Ok(window_id) = rustspace::sys_create_window(600, 400, 20, 50) else {
        rustspace::println!("shell: create_window failed");
        rustspace::sys_exit(1)
    };
    rustspace::println!("shell: window id={}", window_id);

    let Ok(pixels) = rustspace::sys_map_window_buffer(window_id) else {
        rustspace::println!("shell: map_window_buffer failed");
        rustspace::sys_exit(1)
    };
    let mut window_info = WindowInfo::zeroed();
    if !sys_get_window_info(window_id, &mut window_info) {
        rustspace::println!("shell: get_window_info failed");
        rustspace::sys_exit(1)
    }
    let pixels_second = ((pixels as u64) + window_info.front_buffer_offset) as *mut u32;

    let (width, height) = rustspace::sys_get_window_size(window_id).unwrap_or((0, 0));
    if width == 0 || height == 0 {
        rustspace::println!("shell: get_window_size failed");
        rustspace::sys_exit(1)
    }

    rustspace::println!(
        "shell: window {}x{} id={} mapped at {:p}, front: {:p}",
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
        let ok = rustspace::sys_create_circular_buffer(64 * 1024, page_flags, &mut buffer_info);
        if !ok {
            rustspace::println!("shell: cant create circular buffer");
            rustspace::sys_exit(1)
        }
        rustspace::println!(
            "shell: circular buffer base={:#x} view_size={} total={}",
            buffer_info.virtual_base,
            buffer_info.view_size,
            buffer_info.total_virtual_size
        );
        Scrollback::new(buffer_info)
    };

    let surface = unsafe { UserSurface::new(pixels, pixels_second, width, height) };
    let mut shell = Terminal::new(surface, window_id, scrollback);

    rustspace::println!("shell: writing");

    shell.write_line("Welcome to bigOS");
    shell.write_line("=======================");
    shell.write_prompt();

    rustspace::println!("shell: starting loop");

    rustspace::println!(
        "shell: window info: {}x{} event_buffer_vaddr: {}",
        window_info.width,
        window_info.height,
        window_info.event_buffer_vaddr,
    );

    let mut event_reader = unsafe { EventReader::new(window_info.event_buffer_vaddr as usize) };

    rustspace::sys_yield();

    let mut buffers_to_redraw: u8 = 0;
    loop {
        while let Some(event) = event_reader.try_read(window_id) {
            shell.handle_event(event);
        }
        shell.poll_job();

        if shell.needs_redraw {
            shell.needs_redraw = false;
            buffers_to_redraw = WINDOW_BUFFER_COUNT;
        }

        if buffers_to_redraw > 0 {
            shell.render();
            rustspace::sys_present_window(window_id);
            shell.draw_target.swap();
            buffers_to_redraw -= 1;
        } else if shell.job.is_running() {
            rustspace::sys_sleep_ms(JOB_POLL_INTERVAL_MS);
        } else {
            rustspace::sys_wait_for_input();
        }
    }

    // unsafe { rustspace::sys_exit(0) };
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    rustspace::println!("shell: panic");
    rustspace::sys_exit(1)
}
