extern crate alloc;
use linked_list_allocator::LockedHeap;
use x86_64::{
    VirtAddr,
    structures::paging::{
        FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB, mapper::MapToError,
    },
};

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub const HEAP_POINTER: usize = 0x222222220000;
pub const HEAP_SIZE: usize = 1024 * 1024; // 1 MiB

pub fn initialize_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_alloc: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    // We borrow the mapper and frame allocator initialized in kernel_main to map the whole heap
    // read: all pages that cover the heap region
    let page_range = {
        let heap_ptr = VirtAddr::new(HEAP_POINTER as u64);
        let heap_end = heap_ptr + HEAP_SIZE as u64 - 1;
        let heap_first_page: Page<Size4KiB> = Page::containing_address(heap_ptr);
        let heap_last_page: Page<Size4KiB> = Page::containing_address(heap_end);
        Page::range_inclusive(heap_first_page, heap_last_page)
    };

    for page in page_range {
        let frame = frame_alloc
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        unsafe {
            mapper.map_to(page, frame, flags, frame_alloc)?.flush();
        }
    }

    unsafe {
        // init() method already tries to write to our heap, so ensure this is done AFTER mapping heap's pages
        ALLOCATOR.lock().init(HEAP_POINTER, HEAP_SIZE);
    }

    Ok(())
}
