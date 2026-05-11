use crate::main;
use kernel::memory::paging::MemoryMapFrameAllocator;
use kernel::{
    init_globals, interrupts, memory, memory::allocator, serial_println,
    util::cpuinfo::init_cpu_info,
};
use limine::{
    BaseRevision,
    framebuffer::Framebuffer,
    paging::Mode,
    request::{
        EfiMemoryMapRequest, FramebufferRequest, HhdmRequest, MemoryMapRequest, PagingModeRequest,
        RequestsEndMarker, RequestsStartMarker, RsdpRequest,
    },
};
use spin::Mutex;
use spin::Once;

/// Sets the base revision to the latest revision supported by the crate.
/// See specification for further info.
/// Be sure to mark all limine requests with #[used], otherwise they may be removed by the compiler.
#[used]
// The .requests section allows limine to find the requests faster and more safely.
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

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
static EFI_MEMORY_MAP_REQUEST: EfiMemoryMapRequest = EfiMemoryMapRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static PAGING_MODE_REQUEST: PagingModeRequest =
    PagingModeRequest::new().with_mode(Mode::FOUR_LEVEL);

/// Define the stand and end markers for Limine requests.
#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START_MARKER: RequestsStartMarker = RequestsStartMarker::new();
#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END_MARKER: RequestsEndMarker = RequestsEndMarker::new();

pub struct BootInfo {
    pub hhdm_offset: u64,
    pub framebuffer: Mutex<Framebuffer<'static>>,
}

pub static BOOT_INFO: Once<BootInfo> = Once::new();

pub fn boot_info() -> &'static BootInfo {
    unsafe { BOOT_INFO.get().unwrap_unchecked() }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn kmain() -> ! {
    assert!(BASE_REVISION.is_supported());

    serial_println!("BigOS Booted!");

    unsafe { init_cpu_info() };

    // memory::paging::init_acpi_memory_map(rsdp_phys_addr);

    let _efi_memory_map_response = EFI_MEMORY_MAP_REQUEST
        .get_response()
        .expect("Failed to get UEFI memory map response");

    let memory_map_response = MEMORY_MAP_REQUEST
        .get_response()
        .expect("Failed to get memory map response");

    let paging_mode_response = PAGING_MODE_REQUEST
        .get_response()
        .expect("Failed to get paging mode response");
    let _paging_mode = paging_mode_response.mode();

    let hhdm_response = HHDM_REQUEST
        .get_response()
        .expect("Failed to get HHDM respone");
    let hhdm_offset = hhdm_response.offset();

    let rsdp_addr_respone = RSDP_REUEST
        .get_response()
        .expect("Failed to get RSDP address response");
    let rsdp_phys_addr: usize = rsdp_addr_respone.address();
    let rsdp_virt_addr = rsdp_phys_addr + hhdm_offset as usize;

    serial_println!("RSDP physical address: {:#x}", rsdp_phys_addr);
    serial_println!("HHDM offset: {:#x}", hhdm_offset);
    serial_println!("RSDP virtual address: {:#x}", rsdp_virt_addr);

    let framebuffer_response = FRAMEBUFFER_REQUEST
        .get_response()
        .expect("Failed to get framebuffer response");
    let framebuffer = framebuffer_response
        .framebuffers()
        .next()
        .expect("No framebuffer found");

    let boot_info = BootInfo {
        hhdm_offset,
        framebuffer: Mutex::new(framebuffer),
    };

    serial_println!("Boot Info: hhdm_offset: {}", boot_info.hhdm_offset);
    BOOT_INFO.call_once(|| boot_info);

    init_globals();

    let mut mapper = unsafe { memory::paging::init_offset_page_table(hhdm_offset) };
    serial_println!("Offset page table initialized");

    serial_println!("Creating frame_allocator");
    let mut frame_allocator =
        unsafe { MemoryMapFrameAllocator::init(memory_map_response.entries()) };

    memory::paging::map_acpi_regions(
        &mut mapper,
        &mut frame_allocator,
        rsdp_phys_addr,
        hhdm_offset,
    )
    .expect("Failed to map ACPI regions");

    serial_println!("Initializing heap");
    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("Failed to initialize heap");
    serial_println!("Heap initialized");

    unsafe {
        interrupts::init_acpi(
            rsdp_phys_addr,
            hhdm_offset,
            &mut mapper,
            &mut frame_allocator,
        )
    };

    let (kernel_page_table_frame, _) = x86_64::registers::control::Cr3::read();
    let kernel_page_table_phys = kernel_page_table_frame.start_address();
    let user_memory_manager =
        memory::usermem::UserMemoryManager::new(kernel_page_table_phys, hhdm_offset);
    memory::init_memory_globals(frame_allocator, user_memory_manager);
    serial_println!("Global memory managers initialized");

    interrupts::enable_interrupts();

    main()
}
