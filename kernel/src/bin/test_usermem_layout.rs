#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use kernel::data_structures::circular_buffer::CircularBuffer;
use kernel::memory::memory::PAGE_SIZE;
use kernel::process::process::PROCESS_HEAP_VIRT_START;
use kernel::process::process_mem::{FrameOwnership, ProcessMemoryLayout};
use kernel::{boot_common, serial_println_core};
use x86_64::instructions::{hlt, port::Port};
use x86_64::structures::paging::{FrameAllocator, PageTableFlags};
use x86_64::{PhysAddr, VirtAddr};

const HEAP_GROW_COUNT: usize = 200;
const HEAP_GROW_BYTES: usize = 4 * PAGE_SIZE;
const CIRCULAR_BUFFER_BYTES: usize = 16 * PAGE_SIZE;
const BORROWED_TEST_VADDR: u64 = 0x0000_0002_0000_0000;
const BORROWED_TEST_PAGES: usize = 3;
const BORROWED_TEST_MIDDLE_PAGE: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    unsafe {
        let mut port = Port::new(0xF4);
        port.write(exit_code as u32);
    }
    loop {
        hlt();
    }
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    serial_println_core!("PANIC: {:#?}", info);
    exit_qemu(QemuExitCode::Failed);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kmain() -> ! {
    unsafe { boot_common::bsp_early_init() };
    test_main();
}

fn check(condition: bool, description: &str, success: &mut bool) {
    if condition {
        serial_println_core!("ok: {}", description);
    } else {
        serial_println_core!("FAIL: {}", description);
        *success = false;
    }
}

fn test_main() -> ! {
    serial_println_core!("=== test_usermem_layout: start ===");

    let umm = kernel::memory::get_user_mem_mgr();
    let mut success = true;

    let in_use_before = kernel::memory::get_frame_allocator().allocated_bump_frames();
    let mut frame_allocator = kernel::memory::get_frame_allocator();

    let mut layout = ProcessMemoryLayout::new(umm, &mut frame_allocator).expect("address space");

    // many small heap growths merge into a single region
    for _ in 0..HEAP_GROW_COUNT {
        layout
            .allocate_heap(HEAP_GROW_BYTES, umm, &mut frame_allocator)
            .expect("heap growth");
    }
    check(
        layout.regions.len() == 1,
        "heap growths coalesce into one region",
        &mut success,
    );
    check(
        layout.regions[0].start_virt == VirtAddr::new(PROCESS_HEAP_VIRT_START)
            && layout.regions[0].size_bytes == (HEAP_GROW_COUNT * HEAP_GROW_BYTES) as u64,
        "coalesced heap region covers all growths",
        &mut success,
    );

    // circular buffer: user flags are normalized, the second view only borrows
    let regions_before_circular = layout.regions.len();
    let info = CircularBuffer::map_for_user(
        CIRCULAR_BUFFER_BYTES,
        PageTableFlags::WRITABLE,
        umm,
        &mut layout,
        &mut frame_allocator,
    )
    .expect("circular buffer");
    check(
        layout.regions.len() == regions_before_circular + 2,
        "circular buffer adds an owned and a borrowed region",
        &mut success,
    );
    let first_view = layout
        .regions
        .iter()
        .find(|region| region.start_virt.as_u64() == info.virtual_base)
        .expect("first view region");
    let second_view = layout
        .regions
        .iter()
        .find(|region| region.start_virt.as_u64() == info.virtual_base + info.view_size)
        .expect("second view region");
    check(
        first_view.frame_ownership == FrameOwnership::Owned
            && second_view.frame_ownership == FrameOwnership::Borrowed,
        "first view owns the frames, second view borrows",
        &mut success,
    );
    check(
        first_view.page_flags.contains(PageTableFlags::PRESENT)
            && first_view.page_flags.contains(PageTableFlags::NO_EXECUTE)
            && first_view.page_flags.contains(PageTableFlags::WRITABLE),
        "circular flags are normalized to present, writable, no execute",
        &mut success,
    );
    let first_phys = umm
        .translate_user_virt_to_phys(layout.top_page_table_phys, VirtAddr::new(info.virtual_base))
        .expect("first view mapped");
    let second_phys = umm
        .translate_user_virt_to_phys(
            layout.top_page_table_phys,
            VirtAddr::new(info.virtual_base + info.view_size),
        )
        .expect("second view mapped");
    check(
        first_phys == second_phys,
        "both views map the same frame",
        &mut success,
    );

    // invalid circular requests are rejected without side effects
    let regions_before_invalid = layout.regions.len();
    check(
        CircularBuffer::map_for_user(
            0,
            PageTableFlags::WRITABLE,
            umm,
            &mut layout,
            &mut frame_allocator,
        )
        .is_none(),
        "zero sized circular buffer is rejected",
        &mut success,
    );
    check(
        CircularBuffer::map_for_user(
            usize::MAX / 2,
            PageTableFlags::WRITABLE,
            umm,
            &mut layout,
            &mut frame_allocator,
        )
        .is_none(),
        "oversized circular buffer is rejected",
        &mut success,
    );
    check(
        layout.regions.len() == regions_before_invalid,
        "rejected circular requests track nothing",
        &mut success,
    );

    // borrowed frames are not freed when their region is unmapped, and a middle unmap splits
    let mut borrowed_frames: [PhysAddr; BORROWED_TEST_PAGES] =
        [PhysAddr::zero(); BORROWED_TEST_PAGES];
    for frame_slot in borrowed_frames.iter_mut() {
        *frame_slot = frame_allocator
            .allocate_frame()
            .expect("frame")
            .start_address();
    }
    layout
        .map_frames(
            VirtAddr::new(BORROWED_TEST_VADDR),
            &borrowed_frames,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
            FrameOwnership::Borrowed,
            umm,
            &mut frame_allocator,
        )
        .expect("map borrowed frames");
    let regions_with_borrowed = layout.regions.len();

    let free_before_unmap = frame_allocator.free_frame_count();
    let unmapped = layout.unmap_region(
        VirtAddr::new(BORROWED_TEST_VADDR + (BORROWED_TEST_MIDDLE_PAGE * PAGE_SIZE) as u64),
        PAGE_SIZE as u64,
        umm,
        &mut frame_allocator,
    );
    check(unmapped, "unmap_region finds the middle page", &mut success);
    check(
        layout.regions.len() == regions_with_borrowed + 1,
        "unmapping a middle page splits the region",
        &mut success,
    );
    check(
        frame_allocator.free_frame_count() == free_before_unmap,
        "unmapping a borrowed region frees no frame",
        &mut success,
    );
    check(
        !layout.unmap_region(
            VirtAddr::new(BORROWED_TEST_VADDR + (BORROWED_TEST_MIDDLE_PAGE * PAGE_SIZE) as u64),
            PAGE_SIZE as u64,
            umm,
            &mut frame_allocator,
        ),
        "unmapping an absent page reports not found",
        &mut success,
    );

    // the borrowed frames belong to the test, give them back
    for frame_phys in borrowed_frames {
        frame_allocator.deallocate_frame(
            x86_64::structures::paging::PhysFrame::containing_address(frame_phys),
        );
    }

    layout.free_address_space(umm, &mut frame_allocator);
    check(
        !frame_allocator.free_list_has_duplicates(),
        "no frame was freed twice",
        &mut success,
    );
    let in_use_after = frame_allocator.allocated_bump_frames();
    drop(frame_allocator);
    serial_println_core!(
        "test_usermem_layout: in use before={} after={}",
        in_use_before,
        in_use_after
    );
    check(
        in_use_after == in_use_before,
        "every frame is returned after teardown",
        &mut success,
    );

    if success {
        serial_println_core!("=== test_usermem_layout: ok ===");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println_core!("=== test_usermem_layout: FAILED ===");
        exit_qemu(QemuExitCode::Failed);
    }
}
