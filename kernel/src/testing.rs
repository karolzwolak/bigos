use crate::{hlt_loop, serial_print, serial_println};
use core::panic::PanicInfo;
use x86_64::instructions::port::Port;

pub use kernel_macros::test_case;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    // SAFETY: Writing to the QEMU debug-exit port signals the VM to exit cleanly.
    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
    hlt_loop()
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]");
    serial_println!("Error: {}", info);
    exit_qemu(QemuExitCode::Failed)
}

pub trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

pub fn test_runner(tests: &[&dyn Testable]) -> ! {
    serial_println!("Running {} test(s)", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success)
}

pub struct KernelTest {
    pub name: &'static str,
    pub run: fn(),
}

#[linkme::distributed_slice]
pub static KERNEL_TESTS: [KernelTest];

pub fn run_all_tests() -> ! {
    serial_println!("Running {} test(s)", KERNEL_TESTS.len());
    for t in KERNEL_TESTS.iter() {
        serial_print!("{}...\t", t.name);
        (t.run)();
        serial_println!("[ok]");
    }
    exit_qemu(QemuExitCode::Success)
}

/// Initialises GDT, IDT, paging, and heap — sufficient for most integration tests.
pub fn init_with_heap(hhdm_offset: u64, memory_map: &'static [&'static limine::memory_map::Entry]) {
    crate::init_globals();
    let mut mapper = unsafe { crate::memory::paging::init_offset_page_table(hhdm_offset) };
    let mut frame_allocator =
        unsafe { crate::memory::paging::MemoryMapFrameAllocator::init(memory_map) };
    crate::memory::allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("heap init failed");
}
