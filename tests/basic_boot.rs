#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(bigos::testing::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bigos::{hlt_loop, vga_println};
use core::panic::PanicInfo;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    test_main();

    hlt_loop()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    bigos::testing::test_panic_handler(info)
}

#[test_case]
fn test_println() {
    vga_println!("test_println output");
}
