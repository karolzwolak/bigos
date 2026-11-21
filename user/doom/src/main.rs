#![no_std]
#![no_main]

extern crate alloc;

use core::ffi::{c_char, c_int};

use rustspace::{
    ArgSlice, Args, CpuInfoFlat, MAX_ARGC, WindowInfo, sys_create_window, sys_exit,
    sys_get_cpu_info, sys_get_window_info, sys_map_window_buffer,
};

mod keys;
#[path = "libc/mod.rs"]
mod libc;
mod platform;
mod screen;

use platform::{PLATFORM, PlatformState, read_tsc};
use screen::{DOOM_SCREEN_HEIGHT, DOOM_SCREEN_WIDTH};

const SHELL_WINDOW_RIGHT_EDGE_X: i32 = 590;
const DOOM_WINDOW_GAP_X: i32 = 50;
const DOOM_WINDOW_X: i32 = SHELL_WINDOW_RIGHT_EDGE_X + DOOM_WINDOW_GAP_X;
const DOOM_WINDOW_Y: i32 = 50;
const DOOM_PROGRAM_NAME: &core::ffi::CStr = c"doom";
const IWAD_FLAG: &str = "-iwad";
const IWAD_FLAG_C: &core::ffi::CStr = c"-iwad";
const DEFAULT_IWAD_PATH: &core::ffi::CStr = c"/DOOM1.WAD";
const EXTRA_ARGC: usize = 3;
const MAX_DOOM_ARGC: usize = MAX_ARGC + EXTRA_ARGC;

const EXIT_CODE_NO_TSC_FREQUENCY: i32 = 2;
const EXIT_CODE_CREATE_WINDOW_FAILED: i32 = 3;
const EXIT_CODE_MAP_BUFFER_FAILED: i32 = 4;
const EXIT_CODE_WINDOW_INFO_FAILED: i32 = 5;
const EXIT_CODE_PANIC: i32 = 255;

unsafe extern "C" {
    fn doomgeneric_Create(argc: c_int, argv: *mut *mut c_char);
    fn doomgeneric_Tick();
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    rustspace::println!("doom: panic: {}", info);
    sys_exit(EXIT_CODE_PANIC)
}

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {doom_main}",
        "ud2",
        doom_main = sym doom_main,
    )
}

fn fail(message: &str, exit_code: i32) -> ! {
    rustspace::println!("doom: {}", message);
    sys_exit(exit_code)
}

fn build_doom_argv(args: Args, argv: &mut [*mut c_char; MAX_DOOM_ARGC + 1]) -> c_int {
    let mut argc = 0;
    let mut has_iwad = false;

    argv[argc] = DOOM_PROGRAM_NAME.as_ptr() as *mut c_char;
    argc += 1;

    for argument in args.iter() {
        debug_assert!(argc < MAX_DOOM_ARGC);

        has_iwad |= argument == IWAD_FLAG;
        // the kernel NUL-terminates every argument string
        argv[argc] = argument.as_ptr() as *mut c_char;
        argc += 1;
    }

    if !has_iwad {
        argv[argc] = IWAD_FLAG_C.as_ptr() as *mut c_char;
        argv[argc + 1] = DEFAULT_IWAD_PATH.as_ptr() as *mut c_char;
        argc += 2;
    }
    debug_assert!(argc <= MAX_DOOM_ARGC);

    argv[argc] = core::ptr::null_mut();

    argc as c_int
}

extern "C" fn doom_main(argc: usize, argv: *const ArgSlice) -> ! {
    // SAFETY: the unchanged entry values
    let args = unsafe { Args::from_entry(argc, argv) };

    let mut cpu_info = CpuInfoFlat::zeroed();
    if !sys_get_cpu_info(&mut cpu_info) || cpu_info.tsc_frequency_hz == 0 {
        fail("no TSC frequency", EXIT_CODE_NO_TSC_FREQUENCY);
    }

    let Ok(window_id) = sys_create_window(
        DOOM_SCREEN_WIDTH,
        DOOM_SCREEN_HEIGHT,
        DOOM_WINDOW_X,
        DOOM_WINDOW_Y,
    ) else {
        fail("create_window failed", EXIT_CODE_CREATE_WINDOW_FAILED);
    };

    let Ok(back_buffer) = sys_map_window_buffer(window_id) else {
        fail("map_window_buffer failed", EXIT_CODE_MAP_BUFFER_FAILED);
    };

    let mut window_info = WindowInfo::zeroed();
    if !sys_get_window_info(window_id, &mut window_info) {
        fail("get_window_info failed", EXIT_CODE_WINDOW_INFO_FAILED);
    }
    let front_buffer = (back_buffer as u64 + window_info.front_buffer_offset) as *mut u32;

    PLATFORM.fill(PlatformState {
        window_id,
        event_buffer_addr: window_info.event_buffer_vaddr as usize,
        back_buffer,
        front_buffer,
        tsc_frequency_hz: cpu_info.tsc_frequency_hz,
        start_tsc: read_tsc(),
    });

    let mut doom_argv = [core::ptr::null_mut(); MAX_DOOM_ARGC + 1];
    let doom_argc = build_doom_argv(args, &mut doom_argv);

    // SAFETY: argv holds doom_argc valid strings that live as long as the process
    unsafe {
        doomgeneric_Create(doom_argc, doom_argv.as_mut_ptr());

        loop {
            doomgeneric_Tick();
        }
    }
}
