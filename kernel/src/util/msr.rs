use core::arch::asm;

/// Write to a Model-Specific Register (MSR)
/// # Safety
///
/// `msr` must be a valid, writable MSR address accessible at the current privilege level (CPL 0).
/// Passing an invalid MSR will cause a general-protection fault.
pub unsafe fn msr_write(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nostack, preserves_flags)
        );
    }
}

/// # Safety
///
/// `msr` must be a valid, readable MSR address accessible at the current privilege level (CPL 0).
/// Passing an invalid MSR will cause a general-protection fault.
pub unsafe fn msr_read(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nostack, preserves_flags)
        );
    }
    ((high as u64) << 32) | (low as u64)
}
