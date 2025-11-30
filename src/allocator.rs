extern crate alloc;
use alloc::alloc::{GlobalAlloc, Layout};
use core::{
    mem::{align_of, size_of},
    ptr::NonNull,
};
use x86_64::{
    VirtAddr,
    structures::paging::{
        FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB, mapper::MapToError,
    },
};

#[global_allocator]
static ALLOCATOR: Locked<FixedSizeBlockAllocator> = Locked::new(FixedSizeBlockAllocator::new());

pub const HEAP_POINTER: usize = 0x222222220000;
pub const HEAP_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

pub fn initialize_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_alloc: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    let heap_ptr = VirtAddr::new(HEAP_POINTER as u64);
    let heap_end = heap_ptr + HEAP_SIZE_BYTES as u64 - 1;
    let heap_first_page = Page::containing_address(heap_ptr);
    let heap_last_page = Page::containing_address(heap_end);

    let page_range = Page::range_inclusive(heap_first_page, heap_last_page);

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
        // Init_fallback() already tries to write to our heap, so ensure this is done AFTER mapping heap's pages.
        ALLOCATOR
            .lock()
            .init_fallback_alloc(HEAP_POINTER, HEAP_SIZE_BYTES);
    }

    Ok(())
}

/// Wrapper around spin::Mutex to implement GlobalAlloc on a foreign type.
pub struct Locked<T> {
    inner: spin::Mutex<T>,
}

impl<T> Locked<T> {
    pub const fn new(inner: T) -> Self {
        Self {
            inner: spin::Mutex::new(inner),
        }
    }

    pub fn lock(&self) -> spin::MutexGuard<'_, T> {
        self.inner.lock()
    }
}

struct AllocatorListNode {
    next: Option<&'static mut AllocatorListNode>,
}

/// Block sizes used by the fixed-size block allocator.
/// Also used as alignment for each block so they need to be powers of two.
const BLOCK_SIZES: &[usize] = &[8, 16, 32, 64, 128, 256, 512, 1024, 2048];

pub struct FixedSizeBlockAllocator {
    lists: [Option<&'static mut AllocatorListNode>; BLOCK_SIZES.len()],
    /// Fallback allocator for large allocations.
    fallback_allocator: linked_list_allocator::Heap,
}

impl Default for FixedSizeBlockAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl FixedSizeBlockAllocator {
    pub const fn new() -> Self {
        // Needed because static mut ref doesn't implement Clone.
        const EMPTY: Option<&'static mut AllocatorListNode> = None;
        Self {
            lists: [EMPTY; BLOCK_SIZES.len()],
            fallback_allocator: linked_list_allocator::Heap::empty(),
        }
    }

    /// # Safety
    ///
    /// Caller must ensure that the given heap bounds are valid and unused
    pub unsafe fn init_fallback_alloc(&mut self, heap_start: usize, heap_size: usize) {
        unsafe {
            self.fallback_allocator.init(heap_start, heap_size);
        }
    }

    fn fallback_alloc(&mut self, layout: Layout) -> *mut u8 {
        match self.fallback_allocator.allocate_first_fit(layout) {
            Ok(ptr) => ptr.as_ptr(),
            Err(_) => core::ptr::null_mut(),
        }
    }
}

fn list_index(layout: &Layout) -> Option<usize> {
    let required_block_size = layout.size().max(layout.align());
    BLOCK_SIZES.iter().position(|&s| s >= required_block_size)
}

unsafe impl GlobalAlloc for Locked<FixedSizeBlockAllocator> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut allocator = self.lock();
        match list_index(&layout) {
            Some(index) => {
                match allocator.lists[index].take() {
                    Some(node) => {
                        allocator.lists[index] = node.next.take();
                        node as *mut AllocatorListNode as *mut u8
                    }
                    // Allocate a new block.
                    None => {
                        let block_size = BLOCK_SIZES[index];
                        let block_align = block_size;
                        debug_assert!(block_align.is_power_of_two());
                        let layout =
                            core::alloc::Layout::from_size_align(block_size, block_align).unwrap();
                        allocator.fallback_alloc(layout)
                    }
                }
            }
            None => allocator.fallback_alloc(layout),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: alloc::alloc::Layout) {
        let mut allocator = self.lock();
        match list_index(&layout) {
            Some(index) => {
                let new_node = AllocatorListNode {
                    next: allocator.lists[index].take(),
                };
                // Make sure the block has size and alignment required for storing node.
                assert!(size_of::<AllocatorListNode>() <= BLOCK_SIZES[index]);
                assert!(align_of::<AllocatorListNode>() <= BLOCK_SIZES[index]);
                let new_node_ptr = ptr as *mut AllocatorListNode;
                unsafe {
                    new_node_ptr.write(new_node);
                    allocator.lists[index] = Some(&mut *new_node_ptr);
                }
            }
            None => {
                let ptr = NonNull::new(ptr).unwrap();
                unsafe {
                    allocator.fallback_allocator.deallocate(ptr, layout);
                }
            }
        }
    }
}
