#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use kernel::graphics::compositor::{get_compositor, init_compositor};
use kernel::process::elf_loader::{ElfLoadInfo, WINSTRESS_ELF};
use kernel::process::process::ProcessState;
use kernel::process::process_manager::PROCESS_MANAGER;
use kernel::process::scheduler;
use kernel::{boot_common, bsp_init, serial_println_core};
use x86_64::instructions::{hlt, port::Port};

const FRAMEBUFFER_WIDTH: u32 = 640;
const FRAMEBUFFER_HEIGHT: u32 = 480;
const TIMEOUT_MS: u64 = 15000;
const NS_PER_MS: u64 = 1_000_000;
const SETTLE_HLT_COUNT: usize = 10;
// kernel heap growth caused by the window buffers and the compositor window list
const KERNEL_HEAP_GROWTH_MARGIN_FRAMES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    unsafe {
        let mut port = Port::new(0xF4);
        port.write(exit_code as u32);
    }
    loop {
        hlt();
    }
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    serial_println_core!("PANIC: {:#?}", info);
    exit_qemu(QemuExitCode::Failed);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kmain() -> ! {
    unsafe { boot_common::bsp_early_init() };
    test_main();
}

fn test_main() -> ! {
    serial_println_core!("=== test_usermem_windows: start ===");

    kernel::process::shared_state::init_shared_state();
    init_compositor(FRAMEBUFFER_WIDTH, FRAMEBUFFER_HEIGHT);

    bsp_init::wait_for_ap_cores_blocking();

    let mut success = true;

    let before_in_use = kernel::memory::get_frame_allocator().allocated_bump_frames();
    serial_println_core!(
        "test_usermem_windows: before in use frames={}",
        before_in_use
    );

    let elf_info = match ElfLoadInfo::from_elf_data(WINSTRESS_ELF) {
        Ok(info) => info,
        Err(e) => {
            serial_println_core!("FAIL: parse winstress ELF: {:?}", e);
            exit_qemu(QemuExitCode::Failed);
        }
    };

    let pid = {
        let mut pm = PROCESS_MANAGER.lock();
        match pm.create_process_from_elf(0, &elf_info, "winstress", 5) {
            Ok(pid) => pid,
            Err(e) => {
                serial_println_core!("FAIL: create_process_from_elf: {:?}", e);
                exit_qemu(QemuExitCode::Failed);
            }
        }
    };

    let start = kernel::interrupts::system_uptime_ns();
    let mut exit_code: Option<i32> = None;
    let mut terminated = false;

    loop {
        if kernel::interrupts::system_uptime_ns().saturating_sub(start) >= TIMEOUT_MS * NS_PER_MS {
            serial_println_core!("FAIL: winstress pid={} timed out", pid);
            break;
        }

        let state = {
            let pm = PROCESS_MANAGER.lock();
            pm.get_process(pid).ok().map(|p| (p.state, p.exit_code))
        };
        match state {
            Some((ProcessState::Terminated, code)) => {
                terminated = true;
                exit_code = code;
                break;
            }
            Some(_) => {}
            None => {
                terminated = true;
                break;
            }
        }
        hlt();
    }

    if !terminated {
        success = false;
    }
    if exit_code != Some(0) {
        serial_println_core!(
            "FAIL: winstress exit code {:?}, expected Some(0)",
            exit_code
        );
        success = false;
    }

    for _ in 0..SETTLE_HLT_COUNT {
        hlt();
    }

    {
        let compositor = get_compositor();
        let windows = compositor.windows.read();
        let visible_count = windows.iter().filter(|window| window.is_visible).count();
        serial_println_core!(
            "test_usermem_windows: windows={} visible after exit={}",
            windows.len(),
            visible_count
        );
        if visible_count != 0 {
            serial_println_core!("FAIL: windows of the exited process are still visible");
            success = false;
        }

        let event_buffers_left = windows
            .iter()
            .filter(|window| window.event_buffer.is_some())
            .count();
        if event_buffers_left != 0 {
            serial_println_core!(
                "FAIL: {} event buffers were not released",
                event_buffers_left
            );
            success = false;
        }
    }

    {
        let mut pm = PROCESS_MANAGER.lock();
        pm.cleanup_dead();
        if pm.get_process(pid).is_ok() || scheduler::is_pid_queued(pid) {
            serial_println_core!("FAIL: pid {} not reclaimed", pid);
            success = false;
        }
    }

    {
        let frame_allocator = kernel::memory::get_frame_allocator();
        let after_in_use = frame_allocator.allocated_bump_frames();
        serial_println_core!(
            "test_usermem_windows: after in use frames={} (delta {})",
            after_in_use,
            after_in_use as i64 - before_in_use as i64
        );
        if after_in_use > before_in_use + KERNEL_HEAP_GROWTH_MARGIN_FRAMES {
            serial_println_core!("FAIL: frames leaked");
            success = false;
        }
        if frame_allocator.free_list_has_duplicates() {
            serial_println_core!("FAIL: a frame was freed more than once");
            success = false;
        }
    }

    if success {
        serial_println_core!("PASS: windows and circular buffers released");
        serial_println_core!("=== test_usermem_windows: ok ===");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println_core!("=== test_usermem_windows: FAILED ===");
        exit_qemu(QemuExitCode::Failed);
    }
}
