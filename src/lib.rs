#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(custom_test_frameworks)]
#![test_runner(testing::test_runner)]
#![reexport_test_harness_main = "test_main"]

#[cfg(test)]
use bootloader::{BootInfo, entry_point};

pub mod allocator;
pub mod gdt;
pub mod interrupts;
pub mod io;
pub mod memory;
pub mod testing;

pub fn init() {
    gdt::init();
    interrupts::init_idt();
    interrupts::init_hw_interrupts();
}

#[inline(always)]
/// Do nothing loop that tells the CPU to halt until the next interrupt
pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

#[cfg(test)]
entry_point!(test_kernel_main);

#[cfg(test)]
/// Entry point for the `cargo test`
pub fn test_kernel_main(_bootinfo: &'static BootInfo) -> ! {
    init();
    test_main();
    hlt_loop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn lib() {
        serial_println!("hello from lib");
    }
}
