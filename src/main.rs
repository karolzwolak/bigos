#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rdos::testing::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use rdos::{hlt_loop, init, vga_println};

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    vga_println!("{}", info);
    loop {}
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rdos::testing::test_panic_handler(info)
}

entry_point!(kernel_main);

pub fn kernel_main(bootinfo: &'static BootInfo) -> ! {
    use x86_64::{VirtAddr, structures::paging::{Page}};
    use rdos::memory;
    init();

    let phys_mem_offset = VirtAddr::new(bootinfo.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = memory::EmptyFrameAllocator;

    let page = Page::containing_address(VirtAddr::new(0));
    memory::create_mapping(page, &mut mapper, &mut frame_allocator);

    // write the string `New!` to the screen through the new mapping
    let page_ptr: *mut u64 = page.start_address().as_mut_ptr();
    
    unsafe { page_ptr.offset(400).write_volatile(0xf021_f077_f065_f04e); };

    #[cfg(test)]
    test_main();

    vga_println!("Hello, World!");

    hlt_loop()
}

#[cfg(test)]
mod tests {
    use rdos::*;

    #[test_case]
    fn main() {
        serial_println!("hello from main");
    }
}
