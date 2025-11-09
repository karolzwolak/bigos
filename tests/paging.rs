#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rdos::testing::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{BootInfo, entry_point};
use core::{panic::PanicInfo, sync::atomic::AtomicPtr};
use rdos::{hlt_loop, init, memory, vga_println};
use x86_64::{VirtAddr, structures::paging::Page};

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rdos::testing::test_panic_handler(info)
}

static BOOT_INFO: AtomicPtr<BootInfo> = AtomicPtr::new(core::ptr::null_mut());

pub fn bootinfo_access() -> &'static BootInfo {
    let bootinfo_ptr = BOOT_INFO.load(core::sync::atomic::Ordering::SeqCst);
    assert!(!bootinfo_ptr.is_null(), "BootInfo not initialized");
    unsafe { &*bootinfo_ptr }
}

entry_point!(kernel_main);

pub fn kernel_main(bootinfo: &'static BootInfo) -> ! {
    init();
    BOOT_INFO.store(
        bootinfo as *const _ as *mut _,
        core::sync::atomic::Ordering::SeqCst,
    );
    
    test_main();

    vga_println!("Hello, World!");

    hlt_loop()
}

#[test_case]
fn test_paging() {
    let bootinfo_ptr = bootinfo_access();
    let phys_mem_offset = VirtAddr::new(bootinfo_ptr.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator =
        unsafe { memory::BootInfoFrameAllocator::init(&bootinfo_ptr.memory_map) };
    let page = Page::containing_address(VirtAddr::new(0xdeadbeef000));
    memory::create_mapping(page, &mut mapper, &mut frame_allocator);

    // write the string `New!` to the screen through the new mapping
    let page_ptr: *mut u64 = page.start_address().as_mut_ptr();

    unsafe {
        page_ptr.offset(400).write_volatile(0xf021_f077_f065_f04e);
    };
}
