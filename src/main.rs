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
    use rdos::memory::get_active_level_4_table;
    use x86_64::VirtAddr;

    init();

    let phys_mem_offset = VirtAddr::new(bootinfo.physical_memory_offset);
    let _level_4_table = unsafe { get_active_level_4_table(phys_mem_offset) };

    for (i, pt_entry) in _level_4_table.iter().enumerate() {
        if !pt_entry.is_unused() { 
            vga_println!("L4PT entry {}: {:#?}", i, pt_entry); 
        }
    }

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
