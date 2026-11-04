use crate::{
    HHDM_OFFSET,
    memory::memory::{MemoryMapFrameAllocator, PAGE_SIZE},
    process::process_mem::{FrameOwnership, MappedMemoryRegion},
    serial_println, serial_println_core,
};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB, mapper::MapToError, page_table::PageTableEntry,
    },
};

const LEVEL_4_KERNEL_ENTRIES_START: usize = 256;
const LEVEL_4_KERNEL_ENTRIES_END: usize = 512;
const ENTRIES_PER_TABLE: usize = 512;
pub const USER_STACK_TOP: u64 = 0x7FFF_FFFF_F000;
pub const USER_MEM_MAX_ADDRESS: usize = 0x0000_8000_0000_0000;

pub struct UserMemoryManager {
    pub kernel_page_table_phys: PhysAddr,
    pub phys_offset: u64,
}

impl UserMemoryManager {
    pub fn new(kernel_page_table_phys: PhysAddr, phys_offset: u64) -> Self {
        Self {
            kernel_page_table_phys,
            phys_offset,
        }
    }

    pub fn allocate_new_address_space(
        &self,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<PhysAddr, MapToError<Size4KiB>> {
        let new_table_frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let new_table_pml4_phys = new_table_frame.start_address();
        let new_table_pml4_virt = VirtAddr::new(new_table_pml4_phys.as_u64() + self.phys_offset);

        let pml4_table = unsafe { &mut *(new_table_pml4_virt.as_u64() as *mut PageTable) };
        pml4_table.zero();

        let kernel_new_table_pml4_virt =
            VirtAddr::new(self.kernel_page_table_phys.as_u64() + self.phys_offset);
        let kernel_pml4_table =
            unsafe { &*(kernel_new_table_pml4_virt.as_u64() as *const PageTable) };

        for kernel_entry_idx in LEVEL_4_KERNEL_ENTRIES_START..LEVEL_4_KERNEL_ENTRIES_END {
            pml4_table[kernel_entry_idx] = kernel_pml4_table[kernel_entry_idx].clone();
        }

        // After mapping LAPIC, verify it's in the kernel PML4:
        let pml4_virt = VirtAddr::new(self.kernel_page_table_phys.as_u64() + HHDM_OFFSET);
        let pml4 = unsafe { &*(pml4_virt.as_u64() as *const PageTable) };
        let lapic_pml4_idx = ((0xFFFF_FFFF_FF80_0000u64 >> 39) & 0x1FF) as usize;
        if crate::memory::USER_MEMORY_DEBUG_PRINT {
            serial_println!(
                "LAPIC PML4 entry {}: {:?}",
                lapic_pml4_idx,
                pml4[lapic_pml4_idx].flags()
            );
        }

        if crate::memory::USER_MEMORY_DEBUG_PRINT {
            serial_println!(
                "allocate_new_address_space: Created user address space: top-level table at {:?}",
                new_table_pml4_phys
            );
        }

        Ok(new_table_pml4_phys)
    }

    pub fn zero_frame(&self, frame: x86_64::structures::paging::PhysFrame<Size4KiB>) {
        let frame_virt = VirtAddr::new(frame.start_address().as_u64() + self.phys_offset);
        unsafe {
            core::ptr::write_bytes(frame_virt.as_mut_ptr::<u8>(), 0, PAGE_SIZE);
        }
    }

    pub fn translate_user_virt_to_phys(
        &self,
        user_page_table_phys: PhysAddr,
        user_vaddr: VirtAddr,
    ) -> Option<PhysAddr> {
        let pml4_virt = VirtAddr::new(user_page_table_phys.as_u64() + self.phys_offset);
        let pml4 = unsafe { &*(pml4_virt.as_u64() as *const PageTable) };

        let pml4_idx = ((user_vaddr.as_u64() >> 39) & 0x1FF) as usize;
        let pdpt_idx = ((user_vaddr.as_u64() >> 30) & 0x1FF) as usize;
        let pd_idx = ((user_vaddr.as_u64() >> 21) & 0x1FF) as usize;
        let pt_idx = ((user_vaddr.as_u64() >> 12) & 0x1FF) as usize;
        let page_offset = user_vaddr.as_u64() & 0xFFF;

        let pml4_entry = &pml4[pml4_idx];
        if !pml4_entry.flags().contains(PageTableFlags::PRESENT) {
            serial_println!("translate_user_virt_to_phys: PML4 entry not present");
            return None;
        }

        let pdpt_phys = PhysAddr::new(pml4_entry.addr().as_u64());
        let pdpt_virt = VirtAddr::new(pdpt_phys.as_u64() + self.phys_offset);
        let pdpt = unsafe { &*(pdpt_virt.as_u64() as *const PageTable) };

        let pdpt_entry = &pdpt[pdpt_idx];
        if !pdpt_entry.flags().contains(PageTableFlags::PRESENT) {
            serial_println!("translate_user_virt_to_phys: PDPT entry not present");
            return None;
        }

        let pd_phys = PhysAddr::new(pdpt_entry.addr().as_u64());
        let pd_virt = VirtAddr::new(pd_phys.as_u64() + self.phys_offset);
        let pd = unsafe { &*(pd_virt.as_u64() as *const PageTable) };

        let pd_entry = &pd[pd_idx];
        if !pd_entry.flags().contains(PageTableFlags::PRESENT) {
            serial_println!("translate_user_virt_to_phys: PD entry not present");
            return None;
        }

        if pd_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            let phys_addr =
                PhysAddr::new(pd_entry.addr().as_u64() + (user_vaddr.as_u64() & 0x1FFFFF));
            return Some(phys_addr);
        }

        let pt_phys = PhysAddr::new(pd_entry.addr().as_u64());
        let pt_virt = VirtAddr::new(pt_phys.as_u64() + self.phys_offset);
        let pt = unsafe { &*(pt_virt.as_u64() as *const PageTable) };

        let pt_entry = &pt[pt_idx];
        if !pt_entry.flags().contains(PageTableFlags::PRESENT) {
            serial_println!("translate_user_virt_to_phys: PT entry not present");
            return None;
        }

        Some(PhysAddr::new(pt_entry.addr().as_u64() + page_offset))
    }

    /// Read the PageTableFlags of an already-mapped page by walking the page table manually
    /// Returns empty flags if the page is not mapped
    fn get_page_flags(&self, pml4_table_phys: PhysAddr, page: Page<Size4KiB>) -> PageTableFlags {
        let vaddr = page.start_address();
        let pml4_virt = VirtAddr::new(pml4_table_phys.as_u64() + self.phys_offset);
        let pml4 = unsafe { &*(pml4_virt.as_u64() as *const PageTable) };

        let pml4_entry = &pml4[((vaddr.as_u64() >> 39) & 0x1FF) as usize];
        if !pml4_entry.flags().contains(PageTableFlags::PRESENT) {
            return PageTableFlags::empty();
        }
        let pdpt = unsafe {
            &*(VirtAddr::new(pml4_entry.addr().as_u64() + self.phys_offset).as_u64()
                as *const PageTable)
        };

        let pdpt_entry = &pdpt[((vaddr.as_u64() >> 30) & 0x1FF) as usize];
        if !pdpt_entry.flags().contains(PageTableFlags::PRESENT) {
            return PageTableFlags::empty();
        }
        let pd = unsafe {
            &*(VirtAddr::new(pdpt_entry.addr().as_u64() + self.phys_offset).as_u64()
                as *const PageTable)
        };

        let pd_entry = &pd[((vaddr.as_u64() >> 21) & 0x1FF) as usize];
        if !pd_entry.flags().contains(PageTableFlags::PRESENT) {
            return PageTableFlags::empty();
        }
        let pt = unsafe {
            &*(VirtAddr::new(pd_entry.addr().as_u64() + self.phys_offset).as_u64()
                as *const PageTable)
        };

        let pt_entry = &pt[((vaddr.as_u64() >> 12) & 0x1FF) as usize];
        pt_entry.flags()
    }

    pub fn map_virt_mem_region(
        &self,
        pml4_table_phys: PhysAddr,
        virt_addr: VirtAddr,
        size_bytes: u64,
        protection_flags: PageTableFlags,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<(), MapToError<Size4KiB>> {
        if crate::memory::USER_MEMORY_DEBUG_PRINT {
            serial_println!(
                "UserMemoryManager: map_virt_mem_region: mapping vaddr: {:#x}, bytes: {}",
                virt_addr.as_u64(),
                size_bytes
            );
        }
        let new_table_pml4_virt = VirtAddr::new(pml4_table_phys.as_u64() + self.phys_offset);
        let pml4_table = unsafe { &mut *(new_table_pml4_virt.as_u64() as *mut PageTable) };

        let mut user_page_mapper =
            unsafe { OffsetPageTable::new(pml4_table, VirtAddr::new(self.phys_offset)) };

        let user_flags = protection_flags | PageTableFlags::USER_ACCESSIBLE;

        let start_page = Page::containing_address(virt_addr);
        let end_page = Page::containing_address(virt_addr + size_bytes - 1u64);
        for page in Page::range_inclusive(start_page, end_page) {
            let Some(phys_frame) = frame_allocator.allocate_frame() else {
                self.rollback_partial_mapping(
                    pml4_table_phys,
                    start_page,
                    page,
                    protection_flags,
                    frame_allocator,
                );
                return Err(MapToError::FrameAllocationFailed);
            };
            // Freshly allocated frames may be recycled from terminated processes
            self.zero_frame(phys_frame);
            unsafe {
                match user_page_mapper.map_to(page, phys_frame, user_flags, frame_allocator) {
                    Ok(flush) => {
                        flush.flush();
                    }
                    Err(MapToError::PageAlreadyMapped(_existing_frame)) => {
                        frame_allocator.deallocate_frame(phys_frame);
                        // This page was already mapped by a previous segment whose
                        // virtual range overlaps ours at a page boundary.
                        //
                        // Capability flags (PRESENT, WRITABLE, USER_ACCESSIBLE) are
                        // unioned: grant the permission if either segment needs it.
                        let existing_flags = self.get_page_flags(pml4_table_phys, page);

                        // OR all bits together first, then fix up NO_EXECUTE.
                        let mut merged_flags = existing_flags | user_flags;

                        // Only keep NO_EXECUTE if BOTH sides had it set.
                        let both_nx = existing_flags.contains(PageTableFlags::NO_EXECUTE)
                            && user_flags.contains(PageTableFlags::NO_EXECUTE);
                        if !both_nx {
                            merged_flags.remove(PageTableFlags::NO_EXECUTE);
                        }

                        match user_page_mapper.update_flags(page, merged_flags) {
                            Ok(flush) => {
                                flush.flush();
                                if crate::memory::USER_MEMORY_DEBUG_PRINT {
                                    serial_println!(
                                        "  map_virt_mem_region: page {:#x} already mapped, merged flags {:?} | {:?} -> {:?}",
                                        page.start_address().as_u64(),
                                        existing_flags,
                                        user_flags,
                                        merged_flags,
                                    );
                                }
                            }
                            Err(e) => {
                                serial_println!(
                                    "  map_virt_mem_region: page {:#x} already mapped, update_flags failed: {:?}",
                                    page.start_address().as_u64(),
                                    e,
                                );
                            }
                        }
                    }
                    Err(e) => {
                        frame_allocator.deallocate_frame(phys_frame);
                        self.rollback_partial_mapping(
                            pml4_table_phys,
                            start_page,
                            page,
                            protection_flags,
                            frame_allocator,
                        );
                        return Err(e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Undoes the pages mapped by a failed map_virt_mem_region call, freeing their frames
    fn rollback_partial_mapping(
        &self,
        pml4_table_phys: PhysAddr,
        start_page: Page<Size4KiB>,
        failed_page: Page<Size4KiB>,
        protection_flags: PageTableFlags,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) {
        let mapped_bytes = failed_page.start_address() - start_page.start_address();
        let mapped_region = MappedMemoryRegion {
            start_virt: start_page.start_address(),
            size_bytes: mapped_bytes,
            page_flags: protection_flags,
            frame_ownership: FrameOwnership::Owned,
        };
        self.unmap_user_region(pml4_table_phys, &mapped_region, frame_allocator);
    }

    /// Map a specific alloated physical frame into a user address space
    /// Does not allocate a new frame, caller provides the physical address
    pub fn map_specific_frame(
        &self,
        pml4_table_phys: PhysAddr,
        virt_addr: VirtAddr,
        phys_addr: PhysAddr,
        flags: PageTableFlags,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<(), MapToError<Size4KiB>> {
        use x86_64::structures::paging::PhysFrame;

        let new_table_pml4_virt = VirtAddr::new(pml4_table_phys.as_u64() + self.phys_offset);
        let pml4_table = unsafe { &mut *(new_table_pml4_virt.as_u64() as *mut PageTable) };
        let mut user_page_mapper =
            unsafe { OffsetPageTable::new(pml4_table, VirtAddr::new(self.phys_offset)) };

        let page = Page::containing_address(virt_addr);
        let phys_frame = PhysFrame::containing_address(phys_addr);
        let user_flags = flags | PageTableFlags::USER_ACCESSIBLE;

        unsafe {
            //serial_println_core!("map_specific_frame: mapping phys_frame {:x}, page {:x}", phys_frame.start_address(), page.start_address());
            match user_page_mapper.map_to(page, phys_frame, user_flags, frame_allocator) {
                Ok(flush) => {
                    //serial_println_core!("map_specific_frame: mapped successfully");
                    flush.flush();
                    Ok(())
                }
                Err(MapToError::PageAlreadyMapped(existing_frame)) => {
                    debug_assert!(
                        existing_frame == phys_frame,
                        "map_specific_frame: page {:#x} already maps a different frame",
                        page.start_address().as_u64()
                    );
                    if existing_frame != phys_frame {
                        return Err(MapToError::PageAlreadyMapped(existing_frame));
                    }

                    match user_page_mapper.update_flags(page, user_flags) {
                        Ok(flush) => {
                            flush.flush();
                            Ok(())
                        }
                        Err(e) => {
                            serial_println_core!(
                                "map_specific_frame: update_flags failed: {:?}",
                                e
                            );
                            Ok(())
                        }
                    }
                }
                Err(e) => Err(e),
            }
        }
    }

    /// Translate a kernel virtual address (e.g. a heap allocation) to its physical address
    /// by walking the kernel page table. The kernel heap is not HHDM-mapped, its pages
    /// were individually allocated by the frame allocator and mapped by init_heap, so
    /// phys != vaddr - phys_offset. We must walk the page table to find the real frame.
    pub fn translate_kernel_heap_virt_to_phys(&self, vaddr: u64) -> PhysAddr {
        self.translate_user_virt_to_phys(self.kernel_page_table_phys, VirtAddr::new(vaddr))
            .expect("translate_kernel_heap_virt_to_phys: address not mapped in kernel page table")
    }

    /// Unmaps every page of the region, skipping pages that are already absent. Frames are freed
    /// only for Owned regions, so a page shared by two regions is released once, by whichever
    /// region clears its entry first
    pub fn unmap_user_region(
        &self,
        pml4_table_phys: PhysAddr,
        region: &MappedMemoryRegion,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) {
        if region.size_bytes == 0 {
            return;
        }

        let pml4_table_virt = VirtAddr::new(pml4_table_phys.as_u64() + self.phys_offset);
        let pml4_table = unsafe { &mut *(pml4_table_virt.as_u64() as *mut PageTable) };
        let mut user_page_mapper =
            unsafe { OffsetPageTable::new(pml4_table, VirtAddr::new(self.phys_offset)) };

        let start_page = Page::<Size4KiB>::containing_address(region.start_virt);
        let end_page =
            Page::<Size4KiB>::containing_address(region.start_virt + region.size_bytes - 1u64);
        for page in Page::range_inclusive(start_page, end_page) {
            if let Ok((frame, flush)) = user_page_mapper.unmap(page) {
                flush.ignore();

                if region.frame_ownership == FrameOwnership::Owned {
                    frame_allocator.deallocate_frame(frame);
                }
            }
        }

        x86_64::instructions::tlb::flush_all();
    }

    /// Frees every page table of the user half. Run after all regions are unmapped, any leaf
    /// entry still present means a mapping was never tracked
    pub fn free_user_tables(
        &self,
        pml4_phys: PhysAddr,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) {
        let pml4_virt = VirtAddr::new(pml4_phys.as_u64() + self.phys_offset);
        let pml4 = unsafe { &mut *(pml4_virt.as_u64() as *mut PageTable) };

        for pml4_idx in 0..LEVEL_4_KERNEL_ENTRIES_START {
            if !Self::is_table_entry(&pml4[pml4_idx]) {
                continue;
            }
            let pdpt = self.table_at(pml4[pml4_idx].addr());

            for pdpt_idx in 0..ENTRIES_PER_TABLE {
                if !Self::is_table_entry(&pdpt[pdpt_idx]) {
                    continue;
                }
                let pd = self.table_at(pdpt[pdpt_idx].addr());

                for pd_idx in 0..ENTRIES_PER_TABLE {
                    if !Self::is_table_entry(&pd[pd_idx]) {
                        continue;
                    }
                    let pt_phys = pd[pd_idx].addr();
                    let pt = self.table_at(pt_phys);
                    debug_assert!(
                        pt.iter().all(|entry| entry.is_unused()),
                        "free_user_tables: untracked mapping in page table {:#x}",
                        pt_phys.as_u64()
                    );

                    pd[pd_idx].set_unused();
                    frame_allocator.deallocate_frame(PhysFrame::containing_address(pt_phys));
                }

                let pd_phys = pdpt[pdpt_idx].addr();
                pdpt[pdpt_idx].set_unused();
                frame_allocator.deallocate_frame(PhysFrame::containing_address(pd_phys));
            }

            let pdpt_phys = pml4[pml4_idx].addr();
            pml4[pml4_idx].set_unused();
            frame_allocator.deallocate_frame(PhysFrame::containing_address(pdpt_phys));
        }

        x86_64::instructions::tlb::flush_all();
    }

    /// Present entry that points to a lower level table
    fn is_table_entry(entry: &PageTableEntry) -> bool {
        entry.flags().contains(PageTableFlags::PRESENT)
            && !entry.flags().contains(PageTableFlags::HUGE_PAGE)
    }

    fn table_at(&self, table_phys: PhysAddr) -> &'static mut PageTable {
        let table_virt = VirtAddr::new(table_phys.as_u64() + self.phys_offset);
        unsafe { &mut *(table_virt.as_u64() as *mut PageTable) }
    }

    pub fn free_pml4_frame(
        &self,
        pml4_phys: PhysAddr,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) {
        debug_assert!(pml4_phys.as_u64().is_multiple_of(PAGE_SIZE as u64));
        let frame =
            x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(pml4_phys);
        frame_allocator.deallocate_frame(frame);
        unsafe {
            x86_64::instructions::tlb::flush_all();
        }
    }
}

pub fn allocate_zeroed_page(
    frame_allocator: &mut MemoryMapFrameAllocator,
) -> Option<(PhysAddr, VirtAddr)> {
    let frame: x86_64::structures::paging::PhysFrame = frame_allocator.allocate_frame()?;
    let phys_addr = frame.start_address();
    let virt_addr = VirtAddr::new(phys_addr.as_u64() + HHDM_OFFSET);
    unsafe {
        core::ptr::write_bytes(virt_addr.as_mut_ptr::<u8>(), 0, PAGE_SIZE);
    }
    if crate::memory::USER_MEMORY_DEBUG_PRINT {
        serial_println!(
            "allocate_zeroed_page: allocated and zeroed phys addr {:?}",
            phys_addr
        );
    }

    Some((phys_addr, virt_addr))
}
