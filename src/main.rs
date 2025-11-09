#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(bigos::testing::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bigos::{
    hlt_loop, init,
    memory::{self, BootInfoFrameAllocator},
    vga_println,
};
use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use x86_64::VirtAddr;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    vga_println!("{}", info);
    loop {}
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    bigos::testing::test_panic_handler(info)
}

entry_point!(kernel_main);

pub fn kernel_main(boot_info: &'static BootInfo) -> ! {
    init();

    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let _mapper = unsafe { memory::init_offset_page_table(phys_mem_offset) };
    let _frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };

    #[cfg(test)]
    test_main();

    vga_println!("Hello, World!");

    hlt_loop()
}

#[cfg(test)]
mod tests {
    use bigos::*;

    #[test_case]
    fn main() {
        serial_println!("hello from main");
    }
}
