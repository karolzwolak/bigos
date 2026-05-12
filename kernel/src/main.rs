#![no_std]
#![no_main]

mod boot;

use kernel::graphics::compositor::Compositor;
use kernel::{
    graphics::framebuffer::FrameBufferTarget, programs::theophe::Theophe, serial_println,
};
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

    let mut framebuffer_target = FrameBufferTarget::new(boot::boot_info().framebuffer.lock());

    demo::draw_shapes(&mut framebuffer_target);

    let fb_width = framebuffer_target.width as f32;
    let fb_height = framebuffer_target.height as f32;

    //TODO: compositor should own the framebuffer; adjust theophe to work as other processes would, with its own window backbufer
    serial_println!("Framebuffer size: {}x{}", fb_width, fb_height);
    let compositor = Compositor::new();
    let (_window_id, window_buffer) = compositor.create_window(600, 400, 50, 50);

    let (window3_id, window3_buffer) = compositor.create_window(400, 300, 700, 200);
    compositor.set_z_index(window3_id, 5);
    serial_println!("Created window with ID: {}", window3_id);

    demo::render_shaders(&window3_buffer);

    let mut theophe = Theophe::new(window_buffer.back_buffer_mut());
    theophe.write_line("");
    theophe.write_line("  hi");
    theophe.write_line("==========================================================");
    let cpu_info = kernel::util::cpuinfo::get_cpu_info();
    let cpu_info_str = cpu_info.to_pretty_string();
    theophe.write_str(&cpu_info_str);

    theophe.render();

    compositor.focus_window(0);
    compositor.compose(&mut framebuffer_target);

    loop {
        hlt();
    }
}
