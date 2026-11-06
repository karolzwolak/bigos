use crate::interrupts::{
    self, get_lapic_base_addr, get_lapic_base_addr_phys, init_timer_for_core,
    map_local_apic_for_current_core, tsc_read,
};
use crate::memory::{FRAME_ALLOCATOR, get_frame_allocator};
use crate::process::process::INVALID_PID;
use crate::process::{self, CORE_POOL};
use crate::util::apic::APICOffset;
use crate::util::msr::msr_read;
use crate::{HHDM_OFFSET, MAX_CORES, gdt, hlt_loop, serial_println, serial_println_core};
use alloc::vec::Vec;
use bitflags::bitflags;
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};
use limine::mp::MpInfo;
use x86_64::VirtAddr;
use x86_64::instructions::hlt;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, PageTable, Size4KiB, frame,
};

#[derive(Clone, Copy)]
pub struct CpuInfo {
    pub features: CpuFeatureFlags,
    pub cache_line_size: u8,
    pub apic_id: u8,

    pub family: u8,
    pub model: u8,
    pub stepping: u8,
    pub display_family: u16,
    pub display_model: u8,
    pub vendor: CpuVendor,
}

static mut CPU_INFO: CpuInfo = CpuInfo {
    features: CpuFeatureFlags::empty(),
    cache_line_size: 0,
    apic_id: 0,
    family: 0,
    model: 0,
    stepping: 0,
    display_family: 0,
    display_model: 0,
    vendor: CpuVendor::Unknown,
};

const DEFAULT_CPU_INFO: CpuInfo = CpuInfo {
    features: CpuFeatureFlags::empty(),
    cache_line_size: 0,
    apic_id: 0,
    family: 0,
    model: 0,
    stepping: 0,
    display_family: 0,
    display_model: 0,
    vendor: CpuVendor::Unknown,
};

static mut CPU_INFO_PER_CORE: [CpuInfo; MAX_CORES as usize] =
    [DEFAULT_CPU_INFO; MAX_CORES as usize];

#[repr(C, align(16))]
pub struct CpuInfoFlat {
    pub vendor: u8,
    pub family: u8,
    pub model: u8,
    pub stepping: u8,
    pub display_family: u16,
    pub display_model: u8,
    pub cache_line_size: u8,
    pub apic_id: u8,
    pub features: u32,
    pub tsc_frequency_hz: u64,
    pub boot_tsc: u64,
    pub max_cpuid_leaf: u32,
    pub max_extended_cpuid_leaf: u32,
    pub core_count: u8,
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct CpuFeatureFlags: u32 {
        const APIC = 1 << 0;
        const X2APIC = 1 << 1;
        const TSC = 1 << 2; // Time Stamp Counter
        const TSC_DEADLINE = 1 << 3; // TSC Deadline mode
        const PGE = 1 << 4; // Page Global Enable
        const PAT = 1 << 5; // Page Attribute Table
        const SSE = 1 << 6;
        const SSE2 = 1 << 7;
        const SSE3 = 1 << 8;
        const SSE4_1 = 1 << 9;
        const SSE4_2 = 1 << 10;
        const AVX = 1 << 11;
        const AES = 1 << 12;
        const RDRAND = 1 << 13;
        const HYPERVISOR = 1 << 14; // indicates running inside a VM
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
pub enum CpuVendor {
    Unknown = 0,
    Intel = 1,
    AMD = 2,
}

impl core::fmt::Display for CpuVendor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CpuVendor::Intel => write!(f, "Intel"),
            CpuVendor::AMD => write!(f, "AMD"),
            CpuVendor::Unknown => write!(f, "Unknown"),
        }
    }
}

unsafe fn get_vendor(ebx: u32, ecx: u32, edx: u32) -> CpuVendor {
    let vendor_bytes: [u8; 12] = [
        (ebx >> 0) as u8,
        (ebx >> 8) as u8,
        (ebx >> 16) as u8,
        (ebx >> 24) as u8,
        (edx >> 0) as u8,
        (edx >> 8) as u8,
        (edx >> 16) as u8,
        (edx >> 24) as u8,
        (ecx >> 0) as u8,
        (ecx >> 8) as u8,
        (ecx >> 16) as u8,
        (ecx >> 24) as u8,
    ];

    if &vendor_bytes == b"GenuineIntel" {
        CpuVendor::Intel
    } else if &vendor_bytes == b"AuthenticAMD" {
        CpuVendor::AMD
    } else {
        CpuVendor::Unknown
    }
}

unsafe fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    let eax: u32;
    let ebx: u32;
    let ecx: u32;
    let edx: u32;

    unsafe {
        asm!(
            "push rbx",
            "cpuid",
            "mov {ebx:e}, ebx",
            "pop rbx",
            ebx = out(reg) ebx,
            inout("eax") leaf => eax,
            inout("ecx") 0 => ecx,
            out("edx") edx,
            options(nostack, preserves_flags)
        );
    }

    (eax, ebx, ecx, edx)
}

pub fn init_cpu_infos(cpus: &[&MpInfo]) {
    let core_count = cpus.len();
    serial_println!("init_cpu_infos: {} cores", core_count);

    debug_assert!(core_count <= MAX_CORES as usize);
}

pub unsafe fn init_core(core_id: u8) {
    serial_println!("Core {}: init_core", core_id);

    unsafe { enable_sse_for_current_core() };

    process::syscall::init_syscall();
    serial_println!("Core {}: syscall initialized", core_id);

    unsafe { interrupts::init_lapic_for_current_core(core_id) };
    serial_println!("Core {}: LAPIC initialized", core_id);

    unsafe { init_cpu_info_for_core(core_id) };

    unsafe { init_timer_for_core(core_id) };

    serial_println!("Core {}: init_core completed", core_id);
}

/// CR0.MP (bit 1) = 1 - monitor coprocessor
/// CR0.EM (bit 2) = 0 - clear FPU emulation flag
/// CR4.OSFXSR (bit 9) = 1 - OS supports FXSAVE/FXRSTOR
/// CR4.OSXMMEXCPT (bit 10) = 1 - OS handles SSE exceptions
pub unsafe fn enable_sse_for_current_core() {
    unsafe {
        core::arch::asm!(
            "mov rax, cr0",
            "or  rax, 0x2", // set MP
            "and rax, ~0x4",// clear EM
            "mov cr0, rax",
            // set OSFXSR and OSXMMEXCPT
            "mov rax, cr4",
            "or  rax, 0x600",
            "mov cr4, rax",
            out("rax") _,
            options(nostack, nomem),
        );
    }
}

pub unsafe fn init_cpu_info_for_core(core_id: u8) {
    let (max_leaf, vendor_ebx, vendor_ecx, vendor_edx) = unsafe { cpuid(0) };
    serial_println_core!(
        "init_cpu_info: read leaf 0: eax: {:#x}, ebx: {:#x}, ecx: {:#x}, edx: {:#x}",
        max_leaf,
        vendor_ebx,
        vendor_ecx,
        vendor_edx
    );

    assert!(max_leaf >= 1);
    let (feat_eax, feat_ebx, feat_ecx, feat_edx) = unsafe { cpuid(1) };
    serial_println_core!(
        "init_cpu_info: read leaf 1: eax: {:#x} ebx: {:#x}, ecx: {:#x}, edx: {:#x}",
        feat_eax,
        feat_ebx,
        feat_ecx,
        feat_edx
    );

    let base_family = ((feat_eax >> 8) & 0xF) as u8;
    let base_model = ((feat_eax >> 4) & 0xF) as u8;
    let stepping = (feat_eax & 0xF) as u8;
    let display_family = if base_family == 0xF {
        base_family as u16 + (((feat_eax >> 20) & 0xFF) as u16)
    } else {
        base_family as u16
    };
    let display_model = if base_family == 0x6 || base_family == 0xF {
        (((feat_eax >> 16) & 0xF) << 4) | (base_model as u32)
    } else {
        base_model as u32
    } as u8;

    let mut features = CpuFeatureFlags::empty();

    if feat_edx & (1 << 9) != 0 {
        features |= CpuFeatureFlags::APIC;
    }
    if feat_ecx & (1 << 21) != 0 {
        features |= CpuFeatureFlags::X2APIC;
    }
    if feat_edx & (1 << 4) != 0 {
        features |= CpuFeatureFlags::TSC;
    }
    if feat_ecx & (1 << 24) != 0 {
        features |= CpuFeatureFlags::TSC_DEADLINE;
    }
    if feat_edx & (1 << 13) != 0 {
        features |= CpuFeatureFlags::PGE;
    }
    if feat_edx & (1 << 16) != 0 {
        features |= CpuFeatureFlags::PAT;
    }
    if feat_edx & (1 << 25) != 0 {
        features |= CpuFeatureFlags::SSE;
    }
    if feat_edx & (1 << 26) != 0 {
        features |= CpuFeatureFlags::SSE2;
    }
    if feat_ecx & (1 << 0) != 0 {
        features |= CpuFeatureFlags::SSE3;
    }
    if feat_ecx & (1 << 19) != 0 {
        features |= CpuFeatureFlags::SSE4_1;
    }
    if feat_ecx & (1 << 20) != 0 {
        features |= CpuFeatureFlags::SSE4_2;
    }
    if feat_ecx & (1 << 28) != 0 {
        features |= CpuFeatureFlags::AVX;
    }
    if feat_ecx & (1 << 25) != 0 {
        features |= CpuFeatureFlags::AES;
    }
    if feat_ecx & (1 << 30) != 0 {
        features |= CpuFeatureFlags::RDRAND;
    }
    if feat_ecx & (1 << 31) != 0 {
        features |= CpuFeatureFlags::HYPERVISOR;
    }

    let cache_line_size = ((feat_ebx >> 8) & 0xFF) as u8 * 8;
    let apic_id = ((feat_ebx >> 24) & 0xFF) as u8;
    let cpu_family = ((feat_edx >> 8) & 0xF) as u8;
    let cpu_model = ((feat_edx >> 4) & 0xF) as u8;
    let cpu_stepping = (feat_edx & 0xF) as u8;
    let cpu_vendor = unsafe { get_vendor(vendor_ebx, vendor_ecx, vendor_edx) };
    serial_println_core!("CPU Info:");
    serial_println_core!(
        "  Vendor: {}, family={:#x}, (display={:#x}), model={:#x} (display={:#x}), stepping={}",
        cpu_vendor,
        base_family,
        display_family,
        base_model,
        display_model,
        stepping
    );
    serial_println_core!("  Cache Line Size: {}", cache_line_size);
    serial_println_core!("  Features {:#b}", features);

    debug_assert!((core_id as usize) < MAX_CORES as usize);

    unsafe {
        core::ptr::write(
            &raw mut CPU_INFO_PER_CORE[core_id as usize],
            CpuInfo {
                features,
                cache_line_size,
                apic_id,
                family: cpu_family,
                model: cpu_model,
                stepping: cpu_stepping,
                display_family,
                display_model,
                vendor: cpu_vendor,
            },
        );
    }
}

pub fn get_cpu_info_for_core(core_id: u8) -> &'static CpuInfo {
    debug_assert!((core_id as usize) < MAX_CORES as usize);

    unsafe {
        let ptr = &raw const CPU_INFO_PER_CORE[core_id as usize];
        &*ptr
    }
}

pub fn get_current_core_id() -> u8 {
    unsafe {
        let lapic = get_lapic_base_addr();
        let apic_id_reg = lapic.offset(APICOffset::IDr as isize / 4);
        let apic_id = (apic_id_reg.read_volatile() >> 24) as u8;

        apic_id
    }
}

pub fn get_current_cpu_info() -> &'static CpuInfo {
    let core_id = get_current_core_id();
    get_cpu_info_for_core(core_id)
}

use alloc::string::String;
impl CpuFeatureFlags {
    pub fn to_feature_names(&self) -> Vec<&'static str> {
        let mut features = Vec::new();

        if self.contains(CpuFeatureFlags::APIC) {
            features.push("APIC");
        }
        if self.contains(CpuFeatureFlags::X2APIC) {
            features.push("X2APIC");
        }
        if self.contains(CpuFeatureFlags::TSC) {
            features.push("TSC");
        }
        if self.contains(CpuFeatureFlags::TSC_DEADLINE) {
            features.push("TSC Deadline");
        }
        if self.contains(CpuFeatureFlags::PGE) {
            features.push("PGE");
        }
        if self.contains(CpuFeatureFlags::PAT) {
            features.push("PAT");
        }
        if self.contains(CpuFeatureFlags::SSE) {
            features.push("SSE");
        }
        if self.contains(CpuFeatureFlags::SSE2) {
            features.push("SSE2");
        }
        if self.contains(CpuFeatureFlags::SSE3) {
            features.push("SSE3");
        }
        if self.contains(CpuFeatureFlags::SSE4_1) {
            features.push("SSE4.1");
        }
        if self.contains(CpuFeatureFlags::SSE4_2) {
            features.push("SSE4.2");
        }
        if self.contains(CpuFeatureFlags::AVX) {
            features.push("AVX");
        }
        if self.contains(CpuFeatureFlags::AES) {
            features.push("AES");
        }
        if self.contains(CpuFeatureFlags::RDRAND) {
            features.push("RDRAND");
        }
        if self.contains(CpuFeatureFlags::HYPERVISOR) {
            features.push("Hypervisor");
        }

        features
    }
}

impl core::fmt::Display for CpuInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "CPU Information:")?;
        writeln!(f, "  Vendor: {}", self.vendor)?;
        writeln!(
            f,
            "  Family: {}, Model: {}, Stepping: {}",
            self.family, self.model, self.stepping
        )?;
        writeln!(f, "  APIC ID: {}", self.apic_id)?;
        writeln!(f, "  Cache Line Size: {} bytes", self.cache_line_size)?;

        writeln!(f, "  Features:")?;
        let feature_names = self.features.to_feature_names();
        if feature_names.is_empty() {
            writeln!(f, "    <none detected>")?;
        } else {
            // Print features in columns of 4 for readability
            for chunk in feature_names.chunks(4) {
                write!(f, "    ")?;
                for (i, feature) in chunk.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{:<12}", feature)?;
                }
                writeln!(f)?;
            }
        }

        Ok(())
    }
}

impl CpuInfo {
    pub fn to_pretty_string(&self) -> String {
        use core::fmt::Write;
        let mut s = String::new();
        write!(&mut s, "  {}", self).unwrap();
        s
    }

    pub fn to_compact_string(&self) -> String {
        use core::fmt::Write;
        let mut s = String::new();
        let features: Vec<&str> = self.features.to_feature_names();
        let features_str = if features.is_empty() {
            String::from("none")
        } else {
            features.join("")
        };
        write!(
            &mut s,
            "CPU: {} (Family {}, Model {}), APIC ID: {}, Cache: {}B, Features: [{}]",
            self.vendor, self.family, self.model, self.apic_id, self.cache_line_size, features_str
        )
        .unwrap();
        s
    }
}

pub unsafe extern "C" fn ap_core_from_limine_entry_point(cpu: &MpInfo) -> ! {
    //hlt_loop();

    let proc_id = cpu.processor_id as u8;
    let lapic_id = cpu.lapic_id as u8;
    if proc_id == 0 {
        serial_println!("BSP core entered AP entry point, this should never happen!");
        loop {
            core::arch::asm!("hlt");
        }
    }

    let lapic_base_addr = get_lapic_base_addr_phys();
    serial_println!(
        "AP core entry point reached for APIC ID {} (CPU {})",
        lapic_id,
        proc_id
    );
    serial_println!(
        "AP core {}: LAPIC base physical address: {:#x}",
        proc_id,
        lapic_base_addr
    );
    use x86_64::registers::control::Cr3;
    let (active_pml4_frame, _) = Cr3::read();
    serial_println!(
        "AP core {}: Active PML4 frame: {:#x}",
        proc_id,
        active_pml4_frame.start_address().as_u64()
    );

    serial_println!(
        "AP core {} (LAPIC ID {}) starting initialization...",
        proc_id,
        lapic_id
    );

    gdt::init_core_gdt(proc_id);
    serial_println!("Core {}: GDT loaded", proc_id);
    interrupts::load_idt();
    serial_println!("Core {}: IDT loaded", proc_id);

    unsafe { init_core(proc_id) };

    serial_println!("Core {}: Enabling interrupts...", proc_id);
    interrupts::enable_interrupts();
    serial_println!("Core {}: Interrupts enabled", proc_id);

    crate::AP_CORES_READY.fetch_add(1, core::sync::atomic::Ordering::Release);
    serial_println!("Core {}: ready, entering scheduler loop", proc_id);

    process::scheduler::run_on_core_loop(proc_id);
    // loop {
    //     core::arch::asm!("hlt");
    // }
}

impl CpuInfoFlat {
    pub fn from_kernel_info(
        cpu_info: &CpuInfo,
        tsc_freq: u64,
        boot_tsc: u64,
        core_count: u8,
    ) -> Self {
        let mut flat = CpuInfoFlat {
            vendor: cpu_info.vendor as u8,
            family: cpu_info.family,
            model: cpu_info.model,
            stepping: cpu_info.stepping,
            display_family: cpu_info.display_family,
            display_model: cpu_info.display_model,
            cache_line_size: cpu_info.cache_line_size,
            apic_id: cpu_info.apic_id,
            features: cpu_info.features.bits(),
            tsc_frequency_hz: tsc_freq,
            boot_tsc,
            max_cpuid_leaf: 0,
            max_extended_cpuid_leaf: 0,
            core_count,
        };

        flat
    }
}
