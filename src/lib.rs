#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(custom_test_frameworks)]
#![test_runner(testing::test_runner)]
#![reexport_test_harness_main = "test_main"]

pub mod gdt;
pub mod interrupts;
pub mod io;
pub mod testing;

pub fn init() {
    gdt::init();
    interrupts::init_idt();
}

/// Entry point for `cargo test`
#[cfg(test)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    init();
    test_main();
    loop {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn lib() {
        serial_println!("hello from lib");
    }
}
