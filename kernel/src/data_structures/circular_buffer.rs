use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{FrameAllocator, Page, PageTableFlags, PhysFrame, Size4KiB},
};

use crate::{
    memory::{
        memory::{MemoryMapFrameAllocator, PAGE_SIZE, align_up},
        usermem::UserMemoryManager,
    },
    process::{
        process::PROCESS_USER_VADDR_ALLOC_MAX_SIZE,
        process_mem::{FrameOwnership, ProcessMemoryLayout},
    },
};

// both views together must fit in the reservation window
const CIRCULAR_BUFFER_MAX_VIEW_BYTES: u64 = PROCESS_USER_VADDR_ALLOC_MAX_SIZE / 2;

pub struct CircularBuffer {
    data: VirtAddr,
    size: usize,
    curr_write_pos: usize,
    total_written_bytes: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CircularBufferCreateRequest {
    pub size_bytes: usize,
    pub page_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CircularBufferInfo {
    pub virtual_base: u64,
    pub view_size: u64,
    pub total_virtual_size: u64,
}

impl CircularBuffer {
    /// Maps size_bytes of fresh zeroed frames twice, back to back, so a read or write running
    /// past the end of the first view wraps onto the start. The first view owns the frames,
    /// the second one only borrows them. Everything is rolled back on failure
    pub fn map_for_user(
        size_bytes: usize,
        page_flags: PageTableFlags,
        user_memory_manager: &UserMemoryManager,
        proces_memory_layout: &mut ProcessMemoryLayout,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Option<CircularBufferInfo> {
        if size_bytes == 0 || size_bytes as u64 > CIRCULAR_BUFFER_MAX_VIEW_BYTES {
            return None;
        }

        let data_size = align_up(size_bytes as u64, PAGE_SIZE as u64) as usize;
        let total_virt_size = 2 * data_size;

        let mut user_flags =
            PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
        if page_flags.contains(PageTableFlags::WRITABLE) {
            user_flags |= PageTableFlags::WRITABLE;
        }

        let previous_next_alloc_vaddr = proces_memory_layout.next_alloc_vaddr;
        let virt_view_base = proces_memory_layout.allocate_virtual_range(total_virt_size)?;
        let virt_second_view_base = virt_view_base + data_size as u64;

        let frame_count = data_size / PAGE_SIZE;
        let mut frames: Vec<PhysAddr> = Vec::with_capacity(frame_count);
        for _ in 0..frame_count {
            let Some(frame) = frame_allocator.allocate_frame() else {
                Self::free_frames(&frames, frame_allocator);
                proces_memory_layout.next_alloc_vaddr = previous_next_alloc_vaddr;
                return None;
            };
            user_memory_manager.zero_frame(frame);
            frames.push(frame.start_address());
        }

        if proces_memory_layout
            .map_frames(
                virt_view_base,
                &frames,
                user_flags,
                FrameOwnership::Owned,
                user_memory_manager,
                frame_allocator,
            )
            .is_err()
        {
            Self::free_frames(&frames, frame_allocator);
            proces_memory_layout.next_alloc_vaddr = previous_next_alloc_vaddr;
            return None;
        }

        if proces_memory_layout
            .map_frames(
                virt_second_view_base,
                &frames,
                user_flags,
                FrameOwnership::Borrowed,
                user_memory_manager,
                frame_allocator,
            )
            .is_err()
        {
            // the first view owns the frames, unmapping it frees them
            proces_memory_layout.unmap_region(
                virt_view_base,
                data_size as u64,
                user_memory_manager,
                frame_allocator,
            );
            proces_memory_layout.next_alloc_vaddr = previous_next_alloc_vaddr;
            return None;
        }

        Some(CircularBufferInfo {
            virtual_base: virt_view_base.as_u64(),
            view_size: data_size as u64,
            total_virtual_size: total_virt_size as u64,
        })
    }

    fn free_frames(frames: &[PhysAddr], frame_allocator: &mut MemoryMapFrameAllocator) {
        for frame_phys in frames {
            frame_allocator.deallocate_frame(PhysFrame::containing_address(*frame_phys));
        }
    }
}
