#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rdos::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use rdos::vga_println;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    test_main();

    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rdos::test_panic_handler(info)
}

#[test_case]
fn test_println() {
    vga_println!("test_println output");
}
