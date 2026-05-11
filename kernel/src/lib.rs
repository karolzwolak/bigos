#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(portable_simd)]

pub mod memory;
extern crate alloc;
pub mod data_structures;
pub mod filesystem;
pub mod gdt;
pub mod graphics;
pub mod interrupts;
pub mod io;
pub mod process;
pub mod programs;
pub mod testing;
pub mod util;

pub use alloc::string::String;

extern crate lazy_static;

pub fn init_globals() {
    gdt::init();
    interrupts::init_idt();
}

#[inline(always)]
/// Do nothing loop that tells the CPU to halt until the next interrupt
pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
