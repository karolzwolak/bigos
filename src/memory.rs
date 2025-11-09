use bootloader::bootinfo::*;
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB},
};

pub const PAGE_SIZE: usize = 4096;

/// Initialize a new OffsetPageTable.
///
/// # Safety
///
/// - Memory at `phys_mem_offset` must be mapped to virtual memory.
/// - This function must only be called once to avoid multiple mutable references.
pub unsafe fn init_offset_page_table(phys_mem_offset: VirtAddr) -> OffsetPageTable<'static> {
    unsafe {
        let l4_table = active_level_4_table(phys_mem_offset);

        OffsetPageTable::new(l4_table, phys_mem_offset)
    }
}

/// Returns a mutable reference to the active level 4 table.
///
/// # Safety
///
/// - Memory at `phys_mem_offset` must be mapped to virtual memory.
/// - This function must only be called once to avoid multiple mutable references.
unsafe fn active_level_4_table(phys_mem_offset: VirtAddr) -> &'static mut PageTable {
    let (level_4_table_frame, _flags) = Cr3::read();
    let phys = level_4_table_frame.start_address();
    let virt = phys_mem_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    unsafe { &mut *page_table_ptr }
}

/// Provides usable frames from the bootloader's memory map.
pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
    next: usize,
}

impl BootInfoFrameAllocator {
    /// # Safety
    ///
    /// - `memory_map` must be valid.
    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
        }
    }

    /// Returns an iterator over the usable frames specified in the memory map.
    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        let regions = self.memory_map.iter();
        let usable_regions = regions.filter(|r| r.region_type == MemoryRegionType::Usable);
        let addr_ranges = usable_regions.map(|r| r.range.start_addr()..r.range.end_addr());
        let frame_addresses = addr_ranges.flat_map(|r| r.step_by(PAGE_SIZE));

        frame_addresses.map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;

        frame
    }
}
