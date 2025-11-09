#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rdos::testing::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use rdos::{hlt_loop, init, vga_println};

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    vga_println!("{}", info);
    loop {}
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rdos::testing::test_panic_handler(info)
}

entry_point!(kernel_main);

pub fn kernel_main(_bootinfo: &'static BootInfo) -> ! {
    init();

    #[cfg(test)]
    test_main();

    vga_println!("Hello, World!");

    hlt_loop()
}

#[cfg(test)]
mod tests {
    use rdos::*;

    #[test_case]
    fn main() {
        serial_println!("hello from main");
    }
    
}
