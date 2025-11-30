#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(bigos::testing::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;
use alloc::boxed::Box;
use alloc::vec::Vec;
use bigos::{allocator::HEAP_SIZE_BYTES, hlt_loop};
use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    bigos::testing::test_panic_handler(info)
}

entry_point!(main);

fn main(_bootinfo: &'static BootInfo) -> ! {
    use bigos::{allocator, memory};
    use x86_64::VirtAddr;

    bigos::init();
    let phys_mem_offset = VirtAddr::new(_bootinfo.physical_memory_offset);
    let mut mapper = unsafe { memory::init_offset_page_table(phys_mem_offset) };
    let mut frame_allocator =
        unsafe { memory::BootInfoFrameAllocator::init(&_bootinfo.memory_map) };
    allocator::initialize_heap(&mut mapper, &mut frame_allocator)
        .expect("failed to initialize heap");

    test_main();
    hlt_loop();
}

#[test_case]
fn simple_allocation() {
    let heap_value = Box::new(42);
    let heap_value_2 = Box::new(1000);
    assert_eq!(*heap_value, 42);
    assert_eq!(*heap_value_2, 1000);
}

#[test_case]
fn large_vector() {
    let n = 1000;
    let mut vec = Vec::new();
    for i in 0..n {
        vec.push(i);
    }
    assert_eq!(vec.iter().sum::<u64>(), (n - 1) * n / 2);
}

#[test_case]
fn large_num_of_boxes() {
    use alloc::boxed::Box;
    let long = Box::new(1);
    for i in 0..HEAP_SIZE_BYTES - 1 {
        let x = Box::new(i);
        assert_eq!(*x, i);
    }
    assert_eq!(*long, 1);
}

#[test_case]
fn no_leak() {
    let max_size = HEAP_SIZE_BYTES;
    let alloc_size = max_size / 2;
    for _ in 0..1000 {
        let _ = Vec::<u8>::with_capacity(alloc_size);
    }
}
