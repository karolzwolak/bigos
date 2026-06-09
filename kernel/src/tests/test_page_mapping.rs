#![no_std]
#![no_main]

extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    LIMINE_BASE_REVISION, init_globals,
    memory::paging::{MemoryMapFrameAllocator, init_offset_page_table},
    serial_print, serial_println,
    testing::{QemuExitCode, exit_qemu, test_panic_handler},
};
use limine::{
    BaseRevision, RequestsEndMarker, RequestsStartMarker,
    request::{HhdmRequest, MemmapRequest},
};
use x86_64::{
    VirtAddr,
    structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB},
};

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START: RequestsStartMarker = RequestsStartMarker::new();
#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::with_revision(LIMINE_BASE_REVISION);
#[used]
#[unsafe(link_section = ".requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();
#[used]
#[unsafe(link_section = ".requests")]
static MEMORY_MAP_REQUEST: MemmapRequest = MemmapRequest::new();
#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END: RequestsEndMarker = RequestsEndMarker::new();

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    assert!(BASE_REVISION.is_supported());
    let hhdm_offset = HHDM_REQUEST.response().expect("no HHDM").offset;
    let memory_map = MEMORY_MAP_REQUEST
        .response()
        .expect("no memory map")
        .entries();
    init_globals();
    let mut mapper = unsafe { init_offset_page_table(hhdm_offset) };
    let mut frame_allocator = unsafe { MemoryMapFrameAllocator::init(memory_map) };

    serial_print!("test_page_mapping::create_mapping...\t");
    test_create_mapping(&mut mapper, &mut frame_allocator);
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success)
}

fn test_create_mapping(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    let test_virt = VirtAddr::new(0xFFFF_8090_0000_0000);
    let page: Page<Size4KiB> = Page::containing_address(test_virt);

    let frame = frame_allocator.allocate_frame().expect("no free frame");
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

    unsafe {
        mapper
            .map_to(page, frame, flags, frame_allocator)
            .expect("map_to failed")
            .flush();
    }

    unsafe {
        page.start_address()
            .as_mut_ptr::<u64>()
            .write_volatile(0xDEAD_BEEF_CAFE_1234);
        assert_eq!(
            page.start_address().as_ptr::<u64>().read_volatile(),
            0xDEAD_BEEF_CAFE_1234
        );
    }
}
