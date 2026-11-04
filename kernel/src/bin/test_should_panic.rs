#![no_std]
#![no_main]

extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    serial_print, serial_println,
    testing::{QemuExitCode, exit_qemu},
};

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    unsafe { kernel::boot_common::bsp_early_init() };
    should_fail();
    serial_println!("[test did not panic]");
    exit_qemu(QemuExitCode::Failed)
}

fn should_fail() {
    serial_print!("test_should_panic::should_fail...\t");
    assert_eq!(0, 1);
}
