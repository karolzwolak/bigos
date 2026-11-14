use spin::Once;

pub struct BootInfo {
    pub hhdm_offset: u64,
    pub kernel_virtual_base: u64,
    pub kernel_physical_base: u64,
}

pub static BOOT_INFO: Once<BootInfo> = Once::new();

pub fn boot_info() -> &'static BootInfo {
    unsafe { BOOT_INFO.get().unwrap_unchecked() }
}
