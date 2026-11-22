#![no_std]
#![no_main]

extern crate alloc;
extern crate rustspace;

use rustspace::fe::FileExplorer;
use rustspace::fs::path::FsPath;
use rustspace::gfx::surface::UserSurface;
use rustspace::{
    ArgSlice, Args, EventReader, SYS_FOCUS_WINDOW, WindowInfo, sys_create_window,
    sys_destroy_window, sys_exit, sys_get_window_info, sys_get_window_size, sys_map_window_buffer,
    sys_present_window, sys_wait_for_input, syscall1,
};

const WINDOW_WIDTH_PX: u32 = 640;
const WINDOW_HEIGHT_PX: u32 = 400;
const WINDOW_X: i32 = 640;
const WINDOW_Y: i32 = 20;
const EXIT_CODE_WINDOW_FAILED: i32 = 1;
const EXIT_CODE_PANIC: i32 = 255;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    rustspace::println!("fe: panic: {}", info);
    sys_exit(EXIT_CODE_PANIC)
}

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {fe_main}",
        "ud2",
        fe_main = sym fe_main,
    )
}

fn window_failed(what: &str) -> ! {
    rustspace::println!("fe: {} failed", what);
    sys_exit(EXIT_CODE_WINDOW_FAILED)
}

fn start_dir(args: &Args) -> FsPath {
    match args.iter().next() {
        Some(path) => {
            FsPath::resolve_from_cwd(&FsPath::ROOT_DIR_PATH, path).unwrap_or(FsPath::ROOT_DIR_PATH)
        }
        None => FsPath::ROOT_DIR_PATH,
    }
}

extern "C" fn fe_main(argc: usize, argv: *const ArgSlice) -> ! {
    let args = unsafe { Args::from_entry(argc, argv) };

    let Ok(window_id) = sys_create_window(WINDOW_WIDTH_PX, WINDOW_HEIGHT_PX, WINDOW_X, WINDOW_Y)
    else {
        window_failed("create_window")
    };
    let Ok(pixels) = sys_map_window_buffer(window_id) else {
        window_failed("map_window_buffer")
    };
    let mut window_info = WindowInfo::zeroed();
    if !sys_get_window_info(window_id, &mut window_info) {
        window_failed("get_window_info");
    }
    let Ok((width, height)) = sys_get_window_size(window_id) else {
        window_failed("get_window_size")
    };
    debug_assert!(width == window_info.width && height == window_info.height);

    unsafe { syscall1(SYS_FOCUS_WINDOW, window_id as u64) };

    let pixels_second = ((pixels as u64) + window_info.front_buffer_offset) as *mut u32;
    let mut surface = unsafe { UserSurface::new(pixels, pixels_second, width, height) };
    let mut explorer = FileExplorer::new(width, height, &start_dir(&args));
    let mut event_reader = unsafe { EventReader::new(window_info.event_buffer_vaddr as usize) };

    loop {
        while let Some(event) = event_reader.try_read(window_id) {
            explorer.handle_event(event);
        }
        if explorer.is_quit_requested {
            break;
        }

        explorer.update_grid();
        if explorer.grid.render(&mut surface) {
            sys_present_window(window_id);
            surface.swap();
        } else {
            debug_assert!(!explorer.grid.has_stale_rows());
            sys_wait_for_input();
        }
    }

    // SAFETY: the surface is not used after this
    unsafe { sys_destroy_window(window_id) };
    sys_exit(0)
}
