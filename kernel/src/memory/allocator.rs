extern crate alloc;
use crate::serial_println;
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

const ALLOC_DEBUG: bool = false;
macro_rules! alloc_debug {
    ($($arg:tt)*) => {
        if ALLOC_DEBUG {
            serial_println!($($arg)*);
        }
    };
}

#[global_allocator]
pub static ALLOCATOR: MutexWrapper<FixedSizeBlockAllocator> =
    MutexWrapper::new(FixedSizeBlockAllocator::new());

pub const HEAP_POINTER: usize = 0xFFFF_8080_0000_0000;
pub const HEAP_SIZE_BYTES: usize = 16 * 1024 * 1024; // 16 MB

// Wrapper around spin::Mutex to implement GlobalAlloc on a foreign type
pub struct MutexWrapper<T> {
    inner: spin::Mutex<T>,
}

impl<T> MutexWrapper<T> {
    pub const fn new(inner: T) -> Self {
        Self {
            inner: spin::Mutex::new(inner),
        }
    }

    pub fn lock(&self) -> spin::MutexGuard<'_, T> {
        self.inner.lock()
    }
}

/// Block sizes used by the fixed-size block allocator.
/// Also used as alignment for each block so they need to be powers of two.
const BLOCK_SIZES: &[usize] = &[8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096];

struct AllocatorListNode {
    next: Option<&'static mut AllocatorListNode>,
}

pub struct FixedSizeBlockAllocator {
    lists: [Option<&'static mut AllocatorListNode>; BLOCK_SIZES.len()],
    fallback_allocator: linked_list_allocator::Heap,
}

impl Default for FixedSizeBlockAllocator {
    fn default() -> Self {
        Self::new()
    }
}

pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    serial_println!(
        "Initializing heap: HEAP_POINTER={:#x}, HEAP_SIZE_BYTES={:#x}",
        HEAP_POINTER,
        HEAP_SIZE_BYTES
    );

    let heap_ptr = VirtAddr::new(HEAP_POINTER as u64);
    let heap_end = heap_ptr + HEAP_SIZE_BYTES as u64 - 1;
    let heap_start_page = Page::containing_address(heap_ptr);
    let heap_last_page = Page::containing_address(heap_end);
    let page_range = Page::range_inclusive(heap_start_page, heap_last_page);

    serial_println!(
        "Heap pages: first: {:#x} - last: {:#x}",
        heap_start_page.start_address(),
        heap_last_page.start_address()
    );

    for page in page_range {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

        unsafe {
            mapper.map_to(page, frame, flags, frame_allocator)?.flush();
        }
    }
    serial_println!("All heap pages mapped successfully");

    serial_println!("Initializing fallback allocator for the heap");
    unsafe {
        ALLOCATOR
            .lock()
            .init_fallback_allocator(HEAP_POINTER, HEAP_SIZE_BYTES);
    }

    Ok(())
}

impl FixedSizeBlockAllocator {
    pub const fn new() -> Self {
        const EMPTY: Option<&'static mut AllocatorListNode> = None;
        Self {
            lists: [EMPTY; BLOCK_SIZES.len()],
            fallback_allocator: linked_list_allocator::Heap::empty(),
        }
    }

    /// # Safety
    ///
    /// `heap_start` must be a valid virtual address pointing to at least `heap_size` bytes of
    /// writable memory that is exclusively owned by this allocator and not used for anything else.
    pub unsafe fn init_fallback_allocator(&mut self, heap_start: usize, heap_size: usize) {
        serial_println!(
            "Initializing fallback allocator: HEAP_START={:#x}, HEAP_SIZE={:#x}",
            heap_start,
            heap_size
        );
        unsafe {
            self.fallback_allocator.init(heap_start, heap_size);
        }
    }

    fn allocate_with_fallback_allocator(&mut self, layout: Layout) -> *mut u8 {
        match self.fallback_allocator.allocate_first_fit(layout) {
            Ok(ptr) => {
                alloc_debug!("[FALLBACK ALLOC] Success, ptr: {:p}", ptr.as_ptr());
                ptr.as_ptr()
            }
            Err(e) => {
                alloc_debug!("[FALLBACK ALLOC] Failed: {:?}", e);
                core::ptr::null_mut()
            }
        }
    }
}

fn get_block_index(layout: &Layout) -> Option<usize> {
    let size = layout.size().max(layout.align());
    BLOCK_SIZES.iter().position(|&s| s >= size)
}

unsafe impl GlobalAlloc for MutexWrapper<FixedSizeBlockAllocator> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        alloc_debug!(
            "[ALLOC] Request: size={}, align={}",
            layout.size(),
            layout.align()
        );

        let mut allocator = self.lock();
        alloc_debug!("[ALLOC] Lock acquired");

        if let Some(index) = get_block_index(&layout) {
            alloc_debug!(
                "[ALLOC] Using block index={}, size={}",
                index,
                BLOCK_SIZES[index]
            );
            match allocator.lists[index].take() {
                Some(node) => {
                    // reuse an existing block
                    alloc_debug!("[ALLOC] Reusing existing block");
                    allocator.lists[index] = node.next.take();

                    let ptr = node as *mut AllocatorListNode as *mut u8;
                    alloc_debug!("[ALLOC] Returning ptr: {:p}", ptr);

                    ptr
                }

                None => {
                    // allocate a new block
                    alloc_debug!(
                        "[ALLOC] No free block, allocating new block of size {}",
                        BLOCK_SIZES[index]
                    );
                    let block_size = BLOCK_SIZES[index];
                    let align = block_size;

                    let layout = core::alloc::Layout::from_size_align(block_size, align).unwrap();
                    alloc_debug!(
                        "[ALLOC] New block layout: size={}, align={}",
                        layout.size(),
                        layout.align()
                    );

                    let res = allocator.allocate_with_fallback_allocator(layout);
                    alloc_debug!(
                        "[ALLOC] allocate_with_fallback_allocator returned: {:p}",
                        res
                    );

                    res
                }
            }
        } else {
            alloc_debug!(
                "[ALLOC] No block size found, using fallback directly with original layout"
            );
            let res = allocator.allocate_with_fallback_allocator(layout);
            alloc_debug!("[ALLOC] Fallback allocator returned: {:p}", res);
            res
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut allocator = self.lock();

        match get_block_index(&layout) {
            Some(index) => {
                let new_node = AllocatorListNode {
                    next: allocator.lists[index].take(),
                };

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
