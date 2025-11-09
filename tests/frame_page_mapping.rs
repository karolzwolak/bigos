#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rdos::testing::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use rdos::{
    hlt_loop, init,
    testing::{QemuExitCode, exit_qemu},
};
use x86_64::{
    PhysAddr,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTableFlags, PhysFrame, Size4KiB,
    },
};

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rdos::testing::test_panic_handler(info)
}

entry_point!(kernel_main);

pub fn kernel_main(bootinfo: &'static BootInfo) -> ! {
    use rdos::memory;
    use x86_64::{VirtAddr, structures::paging::Page};
    init();

    let phys_mem_offset = VirtAddr::new(bootinfo.physical_memory_offset);
    let mut mapper = unsafe { memory::init_offset_page_table(phys_mem_offset) };
    let mut frame_allocator = unsafe { memory::BootInfoFrameAllocator::init(&bootinfo.memory_map) };

    let page = Page::containing_address(VirtAddr::new(0xdeadbeef000));
    create_mapping(page, &mut mapper, &mut frame_allocator);

    // write the string `New!` to the screen through the new mapping
    let page_ptr: *mut u64 = page.start_address().as_mut_ptr();

    unsafe {
        page_ptr.offset(400).write_volatile(0xf021_f077_f065_f04e);
    };

    exit_qemu(QemuExitCode::Success);
    hlt_loop()
}

fn create_mapping(
    page: Page,
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    let frame = PhysFrame::containing_address(PhysAddr::new(rdos::io::vga::BUFFER_ADDR as u64));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    // this technically breaks the safety contract of `map_to` but it should be fine for this test
    // we might break rust aliasing rules because we are creating a new mutable reference to the VGA buffer
    let map_to_result = unsafe { mapper.map_to(page, frame, flags, frame_allocator) };
    map_to_result.expect("map_to failed").flush();
}
