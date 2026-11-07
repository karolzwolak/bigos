#![no_std]
#![no_main]

extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    HHDM_OFFSET,
    memory::paging::init_offset_page_table,
    serial_print, serial_println,
    testing::{QemuExitCode, exit_qemu, test_panic_handler},
};
use x86_64::{
    VirtAddr,
    structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB},
};

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    unsafe { kernel::boot_common::bsp_early_init() };
    let mut mapper = unsafe { init_offset_page_table(HHDM_OFFSET) };
    let mut frame_allocator = kernel::memory::get_frame_allocator();

    serial_print!("test_page_mapping::create_mapping...\t");
    test_create_mapping(&mut mapper, &mut *frame_allocator);
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success)
}

fn test_create_mapping(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    let test_virt = VirtAddr::new(0xFFFF_8090_0000_0000);
    let page: Page<Size4KiB> = Page::containing_address(test_virt);

    let frame = frame_allocator.allocate_frame().expect("no free frame");
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

    unsafe {
        mapper
            .map_to(page, frame, flags, frame_allocator)
            .expect("map_to failed")
            .flush();
    }

    unsafe {
        page.start_address()
            .as_mut_ptr::<u64>()
            .write_volatile(0xDEAD_BEEF_CAFE_1234);
        assert_eq!(
            page.start_address().as_ptr::<u64>().read_volatile(),
            0xDEAD_BEEF_CAFE_1234
        );
    }
}
