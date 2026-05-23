#![no_std]
#![no_main]

extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    LIMINE_BASE_REVISION, serial_print, serial_println,
    testing::{test_case, test_panic_handler},
};
use limine::{
    BaseRevision, RequestsEndMarker, RequestsStartMarker,
    request::{HhdmRequest, MemmapRequest},
};
use x86_64::instructions::interrupts;

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START: RequestsStartMarker = RequestsStartMarker::new();
#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::with_revision(LIMINE_BASE_REVISION);
#[used]
#[unsafe(link_section = ".requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();
#[used]
#[unsafe(link_section = ".requests")]
static MEMORY_MAP_REQUEST: MemmapRequest = MemmapRequest::new();
#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END: RequestsEndMarker = RequestsEndMarker::new();

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    assert!(BASE_REVISION.is_supported());
    let hhdm_offset = HHDM_REQUEST.response().expect("no HHDM").offset;
    let memory_map = MEMORY_MAP_REQUEST
        .response()
        .expect("no memory map")
        .entries();

    kernel::testing::init_with_heap(hhdm_offset, memory_map);
    kernel::testing::run_all_tests()
}

#[test_case]
fn test_serial_print() {
    serial_print!("test_serial_print output");
    serial_println!();
}

#[test_case]
fn test_breakpoint_exception() {
    interrupts::int3();
}
