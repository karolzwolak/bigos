#![no_std]
#![no_main]

mod boot;

use core::sync::atomic::Ordering;
use kernel::bsp_init::wait_for_ap_cores_blocking;
use kernel::graphics::compositor::{SHELL_RESTART_REQUESTED, get_compositor, init_compositor};
use kernel::testing::{QemuExitCode, exit_qemu};
use kernel::{interrupts, serial_println_core};
use x86_64::instructions::hlt;
extern crate alloc;

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    serial_println_core!("PANIC: {:#?}", info);
    exit_qemu(QemuExitCode::Failed);
}

fn main() -> ! {
    serial_println_core!("Welcome to BigOS!");

    {
        let mut framebuffer_target = kernel::graphics::framebuffer::get_framebuffer();
        let fb = &mut *framebuffer_target;

        let fb_width = fb.width as f32;
        let fb_height = fb.height as f32;

        serial_println_core!("Framebuffer size: {}x{}", fb_width, fb_height);

        kernel::process_start::init_userspace();

        serial_println_core!("Shared state initialized, bigproc started");

        serial_println_core!("Framebuffer size: {}x{}", fb_width, fb_height);

        init_compositor(fb_width as u32, fb_height as u32);
        serial_println_core!("Compositor initialized");

        wait_for_ap_cores_blocking();
        serial_println_core!("AP cores ready, creating userspace processes");
        kernel::filesystem::init_boot_filesystem();
        kernel::process_start::create_userspace_processes();

        serial_println_core!("Created initial userspace processes");

        serial_println_core!("Entering main kernel loop");

        loop {
            //serial_println_core!("loop");
            let time_start = interrupts::system_uptime_ns();
            {
                let mut compositor = get_compositor();
                compositor.process_input_events();
                compositor.compose(fb);
            }
            if SHELL_RESTART_REQUESTED.swap(false, Ordering::AcqRel) {
                kernel::process_start::restart_shell_if_gone();
            }
            let time_end = interrupts::system_uptime_ns();
            let _dt: u64 = time_end - time_start;
            // time_elapsed += dt;
            //serial_println_core!("Loop time: {} ns; {} ms", dt, dt as f32 / 1_000_000.0);
            hlt();
        }
    }
}
