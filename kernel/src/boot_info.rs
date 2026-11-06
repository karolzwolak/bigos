use limine::framebuffer::Framebuffer;
use spin::{Mutex, Once};

pub struct BootInfo {
    pub hhdm_offset: u64,
}

pub static BOOT_INFO: Once<BootInfo> = Once::new();

pub fn boot_info() -> &'static BootInfo {
    unsafe { BOOT_INFO.get().unwrap_unchecked() }
}
