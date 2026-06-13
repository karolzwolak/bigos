#![no_std]
#![no_main]

mod boot;

use core::time;

use kernel::graphics::compositor::Compositor;
use kernel::{programs::theophe::Theophe, serial_println};
use x86_64::instructions::hlt;
extern crate alloc;
use kernel::graphics::demo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    use x86_64::instructions::{nop, port::Port};

    unsafe {
        let mut port = Port::new(0xF4);
        port.write(exit_code as u32);
    }

    loop {
        nop();
    }
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    serial_println!("PANIC: {:#?}", info);
    exit_qemu(QemuExitCode::Failed);
}

fn main() -> ! {
    serial_println!("Welcome to BigOS!");

    kernel::process::syscall::init_syscall_stack();

    let mut framebuffer_target = kernel::graphics::framebuffer::get_framebuffer();
    let fb = &mut *framebuffer_target;

    let fb_width = fb.width as f32;
    let fb_height = fb.height as f32;

    //TODO: compositor should own the framebuffer; adjust theophe to work as other processes would, with its own window backbufer
    serial_println!("Framebuffer size: {}x{}", fb_width, fb_height);
    let compositor = Compositor::new();
    let (_window_id, window_buffer) = compositor.create_window(600, 600, 50, 50);

    let (window2_id, window2_buffer) = compositor.create_window(400, 300, 700, 200);
    compositor.set_z_index(window2_id, 5);
    serial_println!("Created window with ID: {}", window2_id);

    demo::render_shaders(&window2_buffer);

    let mut theophe = Theophe::new(window_buffer.back_buffer_mut());
    theophe.write_line("");
    theophe.write_line("  Welcome to bigOS!");
    theophe.write_line("==========================================================");
    let cpu_info = kernel::util::cpuinfo::get_cpu_info();
    let cpu_info_str = cpu_info.to_pretty_string();
    theophe.write_str(&cpu_info_str);

    demo::init_demo_filesystem();

    theophe.render();

    compositor.focus_window(0);
    let mut last_time_start = kernel::interrupts::system_uptime_ns();
    let mut dynamic_renderer = demo::DynamicRenderer::new();

    loop {
        let time_start = kernel::interrupts::system_uptime_ns();
        let dt = time_start - last_time_start;
        last_time_start = time_start;
        theophe.update();
        if kernel::DEMO_ACTIVE.load(core::sync::atomic::Ordering::Relaxed) {
            let uv = kernel::DEMO_UV_MODE.load(core::sync::atomic::Ordering::Relaxed);
            dynamic_renderer.setup(uv);
            dynamic_renderer.update(&window2_buffer, dt as f32 / 1_000_000.0);
        }
        compositor.compose(&mut framebuffer_target);
    }
}
