#![no_std]
#![no_main]

extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    serial_print, serial_println,
    testing::{Testable, test_panic_handler, test_runner},
};
use limine::BaseRevision;
use limine::request::{HhdmRequest, MemoryMapRequest, RequestsEndMarker, RequestsStartMarker};

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
    test_runner(&[&test_serial_print as &dyn Testable]);
}

fn test_serial_print() {
    serial_print!("test_serial_print output");
    serial_println!();
}
