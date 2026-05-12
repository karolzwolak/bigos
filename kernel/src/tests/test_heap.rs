#![no_std]
#![no_main]

extern crate alloc;
extern crate kernel;

use alloc::{boxed::Box, vec::Vec};
use core::panic::PanicInfo;
use kernel::testing::{test_case, test_panic_handler};
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
    kernel::testing::run_all_tests()
}

#[test_case]
fn simple_allocation() {
    let a = Box::new(42u64);
    let b = Box::new(1000u64);
    assert_eq!(*a, 42);
    assert_eq!(*b, 1000);
}

#[test_case]
fn large_vector() {
    let n: u64 = 1000;
    let mut v = Vec::new();
    for i in 0..n {
        v.push(i);
    }
    assert_eq!(v.iter().sum::<u64>(), (n - 1) * n / 2);
}

#[test_case]
fn large_num_of_boxes() {
    // Verifies the allocator reclaims freed blocks and doesn't corrupt live allocations.
    // Capped well below HEAP_SIZE_BYTES to keep runtime reasonable.
    const ITERATIONS: usize = 10_000;
    let anchor = Box::new(1u64);
    for i in 0..ITERATIONS {
        let x = Box::new(i);
        assert_eq!(*x, i);
    }
    assert_eq!(*anchor, 1);
}

#[test_case]
fn no_leak() {
    // Allocate half the heap repeatedly; verifies the allocator reclaims released memory.
    let alloc_size = kernel::memory::allocator::HEAP_SIZE_BYTES / 2;
    for _ in 0..100 {
        let _ = Vec::<u8>::with_capacity(alloc_size);
    }
}
