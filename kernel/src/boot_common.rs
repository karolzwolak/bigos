use crate::{
    boot_info::{BOOT_INFO, BootInfo},
    gdt::init_core_gdt,
    graphics::compositor::init_input_event_buffer,
    interrupts,
    memory::{self, allocator},
    serial_println, serial_println_core,
};
use crate::{
    interrupts::map_local_apic_for_current_core,
    memory::memory::MemoryMapFrameAllocator,
    process::{CORE_POOL, CorePool},
    util::cpuinfo::{self, init_core, init_cpu_infos},
};
use limine::{
    BaseRevision, RequestsEndMarker, RequestsStartMarker,
    mp::{self, MP_FLAG_X2APIC, MpGotoFunction, MpInfo, MpRespData},
    paging::PagingMode,
    request::{
        EfiMemmapRequest, FramebufferRequest, HhdmRequest, MemmapRequest, MpRequest,
        PagingModeRequest, RsdpRequest,
    },
};

const MP_FLAG_NO_X2APIC: u64 = 0x0;

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START_MARKER: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::with_revision(5u64);

#[used]
#[unsafe(link_section = ".requests")]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static RSDP_REUEST: RsdpRequest = RsdpRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static EFI_MEMMAP_REQUEST: EfiMemmapRequest = EfiMemmapRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static MEMMAP_REQUEST: MemmapRequest = MemmapRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static PAGING_MODE_REQUEST: PagingModeRequest = PagingModeRequest::new(
    PagingMode::X86_64_4LVL,
    PagingMode::X86_64_4LVL,
    PagingMode::X86_64_4LVL,
);

#[used]
#[unsafe(link_section = ".requests")]
static MP_REQUEST: MpRequest = MpRequest::new(MP_FLAG_NO_X2APIC);

#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END_MARKER: RequestsEndMarker = RequestsEndMarker::new();

pub unsafe fn bsp_early_init() {
    assert!(BASE_REVISION.is_supported());
    serial_println!("MofuOS Booted!");

    let _efi_memory_map_response = EFI_MEMMAP_REQUEST
        .response()
        .expect("Failed to get UEFI memory map response");

    let memory_map_response = MEMMAP_REQUEST
        .response()
        .expect("Failed to get memory map response");

    let paging_mode_response = PAGING_MODE_REQUEST
        .response()
        .expect("Failed to get paging mode response");
    let _paging_mode = paging_mode_response.mode;

    let hhdm_response = HHDM_REQUEST.response().expect("Failed to get HHDM respone");
    let hhdm_offset = hhdm_response.offset;

    let rsdp_addr_respone = RSDP_REUEST
        .response()
        .expect("Failed to get RSDP address response");
    let rsdp_virt_addr: usize = rsdp_addr_respone.address as usize;
    let rsdp_phys_addr = rsdp_virt_addr - hhdm_offset as usize;

    serial_println!("HHDM offset: {:#x}", hhdm_offset);
    serial_println!("RSDP physical address: {:#x}", rsdp_phys_addr);
    serial_println!("RSDP virtual address: {:#x}", rsdp_virt_addr);

    let framebuffer_response = FRAMEBUFFER_REQUEST
        .response()
        .expect("Failed to get framebuffer response");
    let framebuffer = framebuffer_response.framebuffers().get(0).unwrap();
    crate::graphics::framebuffer::init_framebuffer(framebuffer);

    let boot_info = BootInfo { hhdm_offset };
    BOOT_INFO.call_once(|| boot_info);
    serial_println!("Boot Info: hhdm_offset: {}", hhdm_offset);

    unsafe {
        init_core_gdt(0);
    }
    interrupts::load_idt();

    let mut mapper = unsafe { memory::memory::init_offset_page_table(hhdm_offset) };
    serial_println!("Offset page table initialized");

    serial_println!("Creating frame_allocator");
    let mut frame_allocator =
        unsafe { MemoryMapFrameAllocator::init(memory_map_response.entries()) };

    use x86_64::registers::control::Cr3;
    let (active_pml4_frame, _) = Cr3::read();
    serial_println!(
        "1 Active PML4 frame: {:#x}",
        active_pml4_frame.start_address().as_u64()
    );

    serial_println!("Initializing heap");
    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("Failed to initialize heap");
    serial_println!("Heap initialized");

    serial_println!("Installing guard pages for kernel stacks");
    {
        use core::mem::{offset_of, size_of};
        serial_println!(
            "GuardedKernelStack size {} guard off {} stack off {}",
            size_of::<crate::gdt::GuardedKernelStack>(),
            offset_of!(crate::gdt::GuardedKernelStack, guard),
            offset_of!(crate::gdt::GuardedKernelStack, stack)
        );
        for i in 0..4 {
            let guard = crate::gdt::rsp0_guard_page(i);
            let (bottom, top) = crate::gdt::rsp0_bounds(i);
            serial_println!(
                "RSP0 core {} guard {:#x} bottom {:#x} top {:#x}",
                i,
                guard.as_u64(),
                bottom.as_u64(),
                top.as_u64()
            );
        }
        for i in 0..4 {
            let guard = crate::gdt::scheduler_guard_page(i);
            let (bottom, top) = crate::gdt::scheduler_bounds(i);
            serial_println!(
                "SCHED core {} guard {:#x} bottom {:#x} top {:#x}",
                i,
                guard.as_u64(),
                bottom.as_u64(),
                top.as_u64()
            );
        }
    }
    crate::gdt::install_guard_pages(&mut frame_allocator);
    serial_println!("Guard pages installed");

    init_input_event_buffer();

    unsafe {
        interrupts::init_acpi(
            rsdp_phys_addr,
            hhdm_offset,
            &mut mapper,
            &mut frame_allocator,
        )
    };

    let mp_response = MP_REQUEST.response().expect("Failed to get MP response");

    serial_println!("MP Response received");
    serial_println!("BSP LAPIC ID: {}", mp_response.bsp_lapic_id);
    serial_println!(
        "uses 2xAPIC? {}",
        mp_response.flags == MP_FLAG_X2APIC as u32
    );

    let cpus = mp_response.cpus();
    let core_count = cpus.len();
    let bsp_lapic_id = mp_response.bsp_lapic_id;

    serial_println!("MP Info:");
    serial_println!("  Total cores: {}", core_count);
    serial_println!("  BSP LAPIC ID: {}", bsp_lapic_id);

    for (i, cpu) in cpus.iter().enumerate() {
        serial_println!(
            "  CPU {}: LAPIC ID={}, Processor ID={}",
            i,
            cpu.lapic_id,
            cpu.processor_id
        );
    }
    unsafe { init_core(0) };

    serial_println!("Mapping lapic for core 0");
    {
        let mut core_pool = CORE_POOL.lock();
        core_pool.init_with_core_count(core_count as u8, cpus);
    }

    interrupts::disable_interrupts();

    let (kernel_page_table_frame, _) = x86_64::registers::control::Cr3::read();
    let kernel_page_table_phys = kernel_page_table_frame.start_address();
    let user_memory_manager =
        memory::usermem::UserMemoryManager::new(kernel_page_table_phys, hhdm_offset);
    serial_println_core!("Global memory managers initialized");

    memory::init_memory_globals(frame_allocator, user_memory_manager);

    unsafe { interrupts::init_tsc_globals() };

    for i in 1..core_count {
        let core_id = i as u8;
        serial_println_core!("Bootstrapping AP core {}", core_id);
        let ap_bootstrap_fn: MpGotoFunction = cpuinfo::ap_core_from_limine_entry_point;
        cpus[i].bootstrap(ap_bootstrap_fn, 0x12345678);
    }

    serial_println_core!("BSP continued execution");
    serial_println_core!("ENABLING INTERRUPTS");
    interrupts::enable_interrupts();
}
