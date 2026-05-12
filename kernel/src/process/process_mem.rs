use crate::memory::paging::{MemoryMapFrameAllocator, PAGE_SIZE};
use crate::memory::usermem::UserMemoryManager;
use crate::serial_println;
use alloc::vec::Vec;
use spin::MutexGuard;
use x86_64::structures::paging::Size4KiB;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::{PhysAddr, VirtAddr, structures::paging::PageTableFlags};

#[derive(Debug, Clone)]
pub struct ProcessMemoryLayout {
    pub top_page_table_phys: PhysAddr,
    pub stack_top: VirtAddr,
    pub stack_size: u64,
    pub heap_start: VirtAddr,
    pub heap_end: VirtAddr,
    pub mapped_regions: Vec<MappedMemoryRegion>,
}

#[derive(Debug, Clone)]
pub struct MappedMemoryRegion {
    pub start_virt: VirtAddr,
    pub size_bytes: u64,
    pub page_flags: PageTableFlags,
}

const fn align_to_page_size(size: u64) -> u64 {
    size.div_ceil(PAGE_SIZE as u64) * PAGE_SIZE as u64
}

impl ProcessMemoryLayout {
    pub fn new(
        address_space_manager: &UserMemoryManager,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<Self, MapToError<Size4KiB>> {
        let top_page_table_phys =
            address_space_manager.allocate_new_address_space(frame_allocator)?;

        Ok(Self {
            top_page_table_phys,
            mapped_regions: Vec::new(),
            stack_top: VirtAddr::new(0),
            stack_size: 0u64,
            heap_start: VirtAddr::new(0x0000_0000_6000_0000),
            heap_end: VirtAddr::new(0x0000_0000_6000_0000),
        })
    }

    pub fn grow_heap(
        &mut self,
        new_heap_end: VirtAddr,
        address_space_manager: MutexGuard<'_, UserMemoryManager>,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<VirtAddr, MapToError<Size4KiB>> {
        if new_heap_end < self.heap_start {
            serial_println!(
                "ProcessMemoryLayout: grow_heap: new_heap_end ({}) < current heap_start ({})",
                new_heap_end.as_u64(),
                self.heap_start.as_u64()
            );
            return Ok(self.heap_end);
        }

        if new_heap_end > self.heap_end {
            let grow_by = align_to_page_size(new_heap_end - self.heap_end);
            if grow_by > 0 {
                let protection_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

                address_space_manager.map_virt_mem_region(
                    self.top_page_table_phys,
                    self.heap_end,
                    grow_by,
                    protection_flags,
                    frame_allocator,
                )?;
                self.mapped_regions.push(MappedMemoryRegion {
                    start_virt: self.heap_end,
                    size_bytes: grow_by,
                    page_flags: protection_flags,
                });
                self.heap_end += grow_by;

                serial_println!(
                    "ProcessMemoryLayout: grow_heap: Grew heap by {} bytes (new heap_end: {})",
                    grow_by,
                    self.heap_end.as_u64()
                );
            }
        } else if new_heap_end < self.heap_end {
            let shrink_by = self.heap_end - new_heap_end;
            self.heap_end = new_heap_end;

            serial_println!(
                "ProcessMemoryLayout: grow_heap: Shrunk heap by {} bytes (new heap_end: {})",
                shrink_by,
                self.heap_end.as_u64()
            );
        }

        Ok(self.heap_end)
    }
}
