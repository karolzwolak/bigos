#![no_std]
#![no_main]

extern crate alloc;
extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    process::elf_loader::{ElfLoadError, ElfLoadInfo, RUST_FIRST_ELF},
    testing::{test_case, test_panic_handler},
};

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
fn parse_valid_elf() {
    let info = ElfLoadInfo::from_elf_data(RUST_FIRST_ELF).expect("valid ELF should parse");
    assert!(info.entry_point > 0, "entry point should be non-zero");
    assert!(
        !info.segments.is_empty(),
        "should have at least one segment"
    );
    assert!(
        info.max_vaddr > info.min_vaddr,
        "vaddr range should be valid"
    );
}

#[test_case]
fn reject_invalid_magic() {
    let bad = [0u8; 64];
    assert!(matches!(
        ElfLoadInfo::from_elf_data(&bad),
        Err(ElfLoadError::ParseError(_))
    ));
}

#[test_case]
fn reject_empty() {
    assert!(matches!(
        ElfLoadInfo::from_elf_data(&[]),
        Err(ElfLoadError::ParseError(_))
    ));
}
