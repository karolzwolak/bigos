#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(bigos::testing::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;
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
    let mut _mapper = unsafe { memory::init_offset_page_table(phys_mem_offset) };
    let mut _frame_allocator =
        unsafe { memory::BootInfoFrameAllocator::init(&_bootinfo.memory_map) };
    allocator::initialize_heap(&mut _mapper, &mut _frame_allocator)
        .expect("Error: failed to initialize heap");

    test_main();
    loop {}
}

#[test_case]
fn simple_allocation() {
    use alloc::boxed::Box;
    let heap_value = Box::new(42);
    let heap_value_2 = Box::new(1000);
    assert_eq!(*heap_value, 42);
    assert_eq!(*heap_value_2, 1000);
}

#[test_case]
fn large_vector() {
    use alloc::vec::Vec;
    let n = 1000;
    let mut vec = Vec::new();
    for i in 0..n {
        vec.push(i);
    }
    assert_eq!(vec.iter().sum::<u64>(), (n - 1) * n / 2);
}
