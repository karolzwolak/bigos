#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(allocator_api)]
#![feature(portable_simd)]
#![allow(warnings, unused)] // TODO: remove this
#![feature(str_from_raw_parts)]

use static_assertions::const_assert;

pub const RUN_THEOPHE: bool = true;
pub const RUN_ODYS: bool = false;
pub const USE_RUST_USER_PROGRAMS: bool = false;
pub const USE_GAME_PROGRAM: bool = false;
pub const RUN_FS_TEST: bool = false;
pub const RUN_FS_CACHED_TEST: bool = false;

pub const HHDM_OFFSET: u64 = 0xFFFF_8000_0000_0000;
pub const MAX_CORES: u8 = 4;
pub const AP_CORE_COUNT: u8 = MAX_CORES - 1;
// NOTE: Hard limit to 64 cores for the core pool availability 64-bit long bitmap
const_assert!(MAX_CORES <= 64);

/// Each AP increments this just before entering run_on_core_loop.
/// BSP spins on this reaching the AP count before launching userspace processes.
pub static AP_CORES_READY: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

pub mod memory;
extern crate alloc;
pub mod asm;
pub mod boot_common;
pub mod boot_info;
pub mod bsp_init;
pub mod data_structures;
pub mod events;
pub mod filesystem;
pub mod gdt;
pub mod graphics;
pub mod interrupts;
pub mod io;
pub mod process;
pub mod process_start;
pub mod programs;
pub mod stack_guard;
pub mod tests_exp;
pub mod trap;
pub mod util;

pub use alloc::string::String;

extern crate lazy_static;

#[inline(always)]
/// Do nothing loop that tells the CPU to halt until the next interrupt
pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
