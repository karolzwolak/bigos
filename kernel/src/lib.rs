#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(portable_simd)]

pub const LIMINE_BASE_REVISION: u64 = 5;

pub mod memory;
extern crate alloc;
pub mod data_structures;
pub mod events;
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
extern crate linkme;

pub fn init_globals() {
    gdt::init();
    interrupts::init_idt();
}

use core::sync::atomic::AtomicBool;
pub static DEMO_ACTIVE: AtomicBool = AtomicBool::new(false);
pub static DEMO_UV_MODE: AtomicBool = AtomicBool::new(false);

#[inline(always)]
/// Do nothing loop that tells the CPU to halt until the next interrupt
pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
