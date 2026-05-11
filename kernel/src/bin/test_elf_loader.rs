#![no_std]
#![no_main]

extern crate alloc;
extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    process::elf_loader::{ElfLoadError, ElfLoadInfo},
    testing::{Testable, test_panic_handler, test_runner},
};
use limine::{
    BaseRevision,
    request::{HhdmRequest, MemoryMapRequest, RequestsEndMarker, RequestsStartMarker},
};

#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();
#[used]
#[unsafe(link_section = ".requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();
#[used]
#[unsafe(link_section = ".requests")]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();
#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START: RequestsStartMarker = RequestsStartMarker::new();
#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END: RequestsEndMarker = RequestsEndMarker::new();

const FIRST_ELF: &[u8] = include_bytes!("../../../target/user/programs/first/first");

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    assert!(BASE_REVISION.is_supported());
    let hhdm_offset = HHDM_REQUEST.get_response().expect("no HHDM").offset();
    let memory_map = MEMORY_MAP_REQUEST
        .get_response()
        .expect("no memory map")
        .entries();
    kernel::testing::init_with_heap(hhdm_offset, memory_map);
    test_runner(&[
        &parse_valid_elf as &dyn Testable,
        &reject_invalid_magic,
        &reject_empty,
    ]);
}

fn parse_valid_elf() {
    let info = ElfLoadInfo::from_elf_data(FIRST_ELF).expect("valid ELF should parse");
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

fn reject_invalid_magic() {
    let bad = [0u8; 64];
    assert!(matches!(
        ElfLoadInfo::from_elf_data(&bad),
        Err(ElfLoadError::ParseError(_))
    ));
}

fn reject_empty() {
    assert!(matches!(
        ElfLoadInfo::from_elf_data(&[]),
        Err(ElfLoadError::ParseError(_))
    ));
}
