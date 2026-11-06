use crate::memory::memory::{MemoryMapFrameAllocator, PAGE_SIZE};
use crate::memory::usermem::UserMemoryManager;
use crate::process::process::{
    PROCESS_HEAP_SIZE_BYTES, PROCESS_HEAP_VIRT_END, PROCESS_HEAP_VIRT_START,
    PROCESS_USER_VADDR_ALLOC_END, PROCESS_USER_VADDR_ALLOC_MAX_SIZE,
    PROCESS_USER_VADDR_ALLOC_START,
};
use crate::{serial_println, serial_println_core};
use alloc::vec::Vec;
use x86_64::structures::paging::Size4KiB;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::{PhysAddr, VirtAddr, structures::paging::PageTableFlags};

/// Who is responsible for freeing the frames behind a mapped region
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOwnership {
    Owned,
    // kernel-owned frames, or a second view of frames owned by another region
    Borrowed,
}

#[derive(Debug, Clone)]
pub struct MappedMemoryRegion {
    pub start_virt: VirtAddr,
    pub size_bytes: u64,
    pub page_flags: PageTableFlags,
    pub frame_ownership: FrameOwnership,
}

impl MappedMemoryRegion {
    pub fn end_virt(&self) -> u64 {
        self.start_virt.as_u64() + self.size_bytes
    }
}

#[derive(Debug, Clone)]
pub struct ProcessMemoryLayout {
    pub top_page_table_phys: PhysAddr,
    pub heap_end: VirtAddr,
    pub next_alloc_vaddr: VirtAddr,
    // sorted by start_virt, page aligned
    pub regions: Vec<MappedMemoryRegion>,
}

#[derive(Debug)]
pub enum ProcessMemoryError {
    OutOfVirtualRange,
    MapTo(MapToError<Size4KiB>),
}

impl From<MapToError<Size4KiB>> for ProcessMemoryError {
    fn from(error: MapToError<Size4KiB>) -> Self {
        Self::MapTo(error)
    }
}

const fn align_to_page_size(size: u64) -> u64 {
    ((size + PAGE_SIZE as u64 - 1) / PAGE_SIZE as u64) * PAGE_SIZE as u64
}

const fn align_down_to_page_size(address: u64) -> u64 {
    address & !(PAGE_SIZE as u64 - 1)
}

impl ProcessMemoryLayout {
    pub fn with_page_table(top_page_table_phys: PhysAddr) -> Self {
        Self {
            top_page_table_phys,
            heap_end: VirtAddr::new(PROCESS_HEAP_VIRT_START),
            next_alloc_vaddr: VirtAddr::new(PROCESS_USER_VADDR_ALLOC_START),
            regions: Vec::new(),
        }
    }

    pub fn new(
        address_space_manager: &UserMemoryManager,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<Self, MapToError<Size4KiB>> {
        let top_page_table_phys =
            address_space_manager.allocate_new_address_space(frame_allocator)?;

        Ok(Self::with_page_table(top_page_table_phys))
    }

    /// Maps zeroed frames owned by the process and rolls back on failure,
    /// start_virt and size_bytes do not have to be page aligned, the covered pages are mapped
    pub fn map_owned_region(
        &mut self,
        start_virt: VirtAddr,
        size_bytes: u64,
        page_flags: PageTableFlags,
        address_space_manager: &UserMemoryManager,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<(), MapToError<Size4KiB>> {
        if size_bytes == 0 {
            return Ok(());
        }

        address_space_manager.map_virt_mem_region(
            self.top_page_table_phys,
            start_virt,
            size_bytes,
            page_flags,
            frame_allocator,
        )?;
        self.track_region(MappedMemoryRegion {
            start_virt,
            size_bytes,
            page_flags,
            frame_ownership: FrameOwnership::Owned,
        });

        Ok(())
    }

    /// Maps explicit frames one per page starting at start_virt. On failure the pages mapped by
    /// this call are unmapped again and the frames are left to the caller
    pub fn map_frames(
        &mut self,
        start_virt: VirtAddr,
        frames: &[PhysAddr],
        page_flags: PageTableFlags,
        frame_ownership: FrameOwnership,
        address_space_manager: &UserMemoryManager,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<(), MapToError<Size4KiB>> {
        debug_assert!(start_virt.is_aligned(PAGE_SIZE as u64));

        if frames.is_empty() {
            return Ok(());
        }

        for (page_index, frame_phys) in frames.iter().enumerate() {
            let page_virt = start_virt + (page_index * PAGE_SIZE) as u64;
            if let Err(error) = address_space_manager.map_specific_frame(
                self.top_page_table_phys,
                page_virt,
                *frame_phys,
                page_flags,
                frame_allocator,
            ) {
                let mapped_region = MappedMemoryRegion {
                    start_virt,
                    size_bytes: (page_index * PAGE_SIZE) as u64,
                    page_flags,
                    frame_ownership: FrameOwnership::Borrowed,
                };
                address_space_manager.unmap_user_region(
                    self.top_page_table_phys,
                    &mapped_region,
                    frame_allocator,
                );

                return Err(error);
            }
        }

        self.track_region(MappedMemoryRegion {
            start_virt,
            size_bytes: (frames.len() * PAGE_SIZE) as u64,
            page_flags,
            frame_ownership,
        });

        Ok(())
    }

    /// True if one tracked region covers the whole range
    pub fn is_range_tracked(&self, start_virt: VirtAddr, size_bytes: u64) -> bool {
        let range_start = start_virt.as_u64();
        let range_end = range_start + size_bytes;

        self.regions.iter().any(|region| {
            region.start_virt.as_u64() <= range_start && region.end_virt() >= range_end
        })
    }

    /// Unmaps a page aligned range lying inside one tracked region and stops tracking it,
    /// frames are freed only if the region owns them. Returns false if no region covers the range
    pub fn unmap_region(
        &mut self,
        start_virt: VirtAddr,
        size_bytes: u64,
        address_space_manager: &UserMemoryManager,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> bool {
        debug_assert!(start_virt.is_aligned(PAGE_SIZE as u64));
        debug_assert!(size_bytes > 0 && size_bytes % PAGE_SIZE as u64 == 0);

        let range_start = start_virt.as_u64();
        let range_end = range_start + size_bytes;
        let Some(region_index) = self.regions.iter().position(|region| {
            region.start_virt.as_u64() <= range_start && region.end_virt() >= range_end
        }) else {
            return false;
        };

        let region = self.regions.remove(region_index);
        let unmapped_region = MappedMemoryRegion {
            start_virt,
            size_bytes,
            page_flags: region.page_flags,
            frame_ownership: region.frame_ownership,
        };
        address_space_manager.unmap_user_region(
            self.top_page_table_phys,
            &unmapped_region,
            frame_allocator,
        );

        // keep the pieces that were not unmapped, in sorted position
        let mut insert_at = region_index;
        if region.start_virt.as_u64() < range_start {
            self.regions.insert(
                insert_at,
                MappedMemoryRegion {
                    size_bytes: range_start - region.start_virt.as_u64(),
                    ..region.clone()
                },
            );
            insert_at += 1;
        }
        if range_end < region.end_virt() {
            self.regions.insert(
                insert_at,
                MappedMemoryRegion {
                    start_virt: VirtAddr::new(range_end),
                    size_bytes: region.end_virt() - range_end,
                    ..region
                },
            );
        }

        true
    }

    /// Inserts the region keeping regions sorted by start, merging with touching or overlapping
    /// neighbours that have equal flags and ownership
    fn track_region(&mut self, region: MappedMemoryRegion) {
        debug_assert!(region.size_bytes > 0);

        let aligned_start = align_down_to_page_size(region.start_virt.as_u64());
        let aligned_end = align_to_page_size(region.end_virt());
        let mut merged = MappedMemoryRegion {
            start_virt: VirtAddr::new(aligned_start),
            size_bytes: aligned_end - aligned_start,
            page_flags: region.page_flags,
            frame_ownership: region.frame_ownership,
        };

        let insert_index = self
            .regions
            .partition_point(|existing| existing.start_virt < merged.start_virt);

        let can_merge = |existing: &MappedMemoryRegion, candidate: &MappedMemoryRegion| {
            existing.page_flags == candidate.page_flags
                && existing.frame_ownership == candidate.frame_ownership
        };

        let mut insert_at = insert_index;
        if insert_at > 0 {
            let previous = &self.regions[insert_at - 1];
            if can_merge(previous, &merged) && previous.end_virt() >= merged.start_virt.as_u64() {
                let merged_end = merged.end_virt().max(previous.end_virt());
                merged.start_virt = previous.start_virt;
                merged.size_bytes = merged_end - merged.start_virt.as_u64();
                self.regions.remove(insert_at - 1);
                insert_at -= 1;
            }
        }

        while insert_at < self.regions.len() {
            let next = &self.regions[insert_at];
            if !(can_merge(next, &merged) && next.start_virt.as_u64() <= merged.end_virt()) {
                break;
            }

            let merged_end = merged.end_virt().max(next.end_virt());
            merged.size_bytes = merged_end - merged.start_virt.as_u64();
            self.regions.remove(insert_at);
        }

        self.regions.insert(insert_at, merged);
    }

    pub fn allocate_heap(
        &mut self,
        size_bytes: usize,
        address_space_manager: &UserMemoryManager,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<VirtAddr, ProcessMemoryError> {
        if size_bytes == 0 {
            return Ok(self.heap_end);
        }

        if size_bytes as u64 > PROCESS_HEAP_SIZE_BYTES {
            serial_println!(
                "ProcessMemoryLayout: allocate_heap: size {} exceeds heap capacity {}",
                size_bytes,
                PROCESS_HEAP_SIZE_BYTES
            );
            return Err(ProcessMemoryError::OutOfVirtualRange);
        }

        let aligned_size = align_to_page_size(size_bytes as u64);
        let old_heap_end = self.heap_end;
        let new_heap_end = old_heap_end + aligned_size;
        self.grow_heap(new_heap_end, address_space_manager, frame_allocator)?;
        Ok(old_heap_end)
    }

    pub fn grow_heap(
        &mut self,
        new_heap_end: VirtAddr,
        address_space_manager: &UserMemoryManager,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<VirtAddr, ProcessMemoryError> {
        if new_heap_end <= self.heap_end {
            return Ok(self.heap_end);
        }

        if new_heap_end > VirtAddr::new(PROCESS_HEAP_VIRT_END) {
            serial_println!(
                "ProcessMemoryLayout: grow_heap: new_heap_end ({}) exceeds heap limit ({})",
                new_heap_end.as_u64(),
                PROCESS_HEAP_VIRT_END
            );
            return Err(ProcessMemoryError::OutOfVirtualRange);
        }

        let grow_by = align_to_page_size(new_heap_end - self.heap_end);
        if grow_by == 0 {
            return Ok(self.heap_end);
        }

        let protection_flags =
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;

        self.map_owned_region(
            self.heap_end,
            grow_by,
            protection_flags,
            address_space_manager,
            frame_allocator,
        )?;
        self.heap_end += grow_by;

        serial_println!(
            "ProcessMemoryLayout: grow_heap: Grew heap by {} bytes (new heap_end: {})",
            grow_by,
            self.heap_end.as_u64()
        );

        Ok(self.heap_end)
    }

    /// Reserves a range in the reservation window without mapping or tracking it, the caller
    /// maps it right away. Ranges are only visible to find_free_gap once mapped
    pub fn allocate_virtual_range(&mut self, size_bytes: usize) -> Option<VirtAddr> {
        if size_bytes as u64 > PROCESS_USER_VADDR_ALLOC_MAX_SIZE {
            return None;
        }
        let aligned_size = align_to_page_size(size_bytes as u64);

        let alloc_vaddr = self.next_alloc_vaddr.as_u64();
        let range_end = alloc_vaddr + aligned_size;

        if range_end > PROCESS_USER_VADDR_ALLOC_END {
            serial_println_core!(
                "allocate_virtual_range: range_end is over the user vaddr alloc end: {}, trying to find a free gap",
                range_end
            );
            return self.find_free_gap(aligned_size as usize);
        }

        self.next_alloc_vaddr = VirtAddr::new(range_end);

        Some(VirtAddr::new(alloc_vaddr))
    }

    /// First gap in the reservation window that fits, relies on regions being sorted by start
    fn find_free_gap(&self, size_bytes: usize) -> Option<VirtAddr> {
        let aligned_size = align_to_page_size(size_bytes as u64);
        if aligned_size == 0 {
            return None;
        }

        let window_start = PROCESS_USER_VADDR_ALLOC_START;
        let window_end = PROCESS_USER_VADDR_ALLOC_END;

        let mut cursor = window_start;
        for region in self.regions.iter() {
            let region_start = region.start_virt.as_u64();
            let region_end = region.end_virt();

            if region_end <= window_start {
                continue;
            }
            if region_start >= window_end {
                break;
            }

            if region_start >= cursor.saturating_add(aligned_size) {
                return Some(VirtAddr::new(cursor));
            }
            cursor = cursor.max(region_end);
        }

        if cursor.saturating_add(aligned_size) <= window_end {
            return Some(VirtAddr::new(cursor));
        }

        None
    }

    pub fn free_address_space(
        &self,
        user_mgr: &UserMemoryManager,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) {
        if self.top_page_table_phys.as_u64() == 0 {
            return;
        }
        let pml4_phys = self.top_page_table_phys;

        if crate::process::PROCESS_DEBUG_PRINT {
            serial_println!(
                "free_address_space: pml4 {:#x} regions {}",
                pml4_phys.as_u64(),
                self.regions.len()
            );
        }

        for region in self.regions.iter() {
            user_mgr.unmap_user_region(pml4_phys, region, frame_allocator);
        }
        user_mgr.free_user_tables(pml4_phys, frame_allocator);
        user_mgr.free_pml4_frame(pml4_phys, frame_allocator);

        if crate::process::PROCESS_DEBUG_PRINT {
            serial_println!(
                "free_address_space: freed pml4 {:#x} free_list now {}",
                pml4_phys.as_u64(),
                frame_allocator.free_frame_count()
            );
        }
    }
}
