#![no_std]
#![no_main]

extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    serial_print, serial_println,
    testing::{test_case, test_panic_handler},
};
use x86_64::instructions::interrupts;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    unsafe { kernel::boot_common::bsp_early_init() };
    kernel::testing::run_all_tests()
}

#[test_case]
fn test_serial_print() {
    serial_print!("test_serial_print output");
    serial_println!();
}

#[test_case]
fn test_breakpoint_exception() {
    interrupts::int3();
}
