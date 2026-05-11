use crate::{memory::paging::MemoryMapFrameAllocator, serial_println};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, Size4KiB,
        mapper::MapToError,
    },
};

const LEVEL_4_KERNEL_ENTRIES_START: usize = 256;
const LEVEL_4_KERNEL_ENTRIES_END: usize = 512;
pub const USER_STACK_TOP: u64 = 0x7FFF_FFFF_F000;

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

        serial_println!(
            "allocate_new_address_space: Created user address space: top-level table at {:?}",
            new_table_pml4_phys
        );

        Ok(new_table_pml4_phys)
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

    pub fn map_virt_mem_region(
        &self,
        pml4_table_phys: PhysAddr,
        virt_addr: VirtAddr,
        size_bytes: u64,
        protection_flags: PageTableFlags,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<(), MapToError<Size4KiB>> {
        serial_println!(
            "UserMemoryManager: map_virt_mem_region: mapping vaddr: {:#x}, bytes: {}",
            virt_addr.as_u64(),
            size_bytes
        );
        let new_table_pml4_virt = VirtAddr::new(pml4_table_phys.as_u64() + self.phys_offset);
        let pml4_table = unsafe { &mut *(new_table_pml4_virt.as_u64() as *mut PageTable) };

        let mut user_page_mapper =
            unsafe { OffsetPageTable::new(pml4_table, VirtAddr::new(self.phys_offset)) };

        let user_flags = protection_flags | PageTableFlags::USER_ACCESSIBLE;

        let start_page = Page::containing_address(virt_addr);
        let end_page = Page::containing_address(virt_addr + size_bytes - 1u64);
        for page in Page::range_inclusive(start_page, end_page) {
            let phys_frame = frame_allocator
                .allocate_frame()
                .ok_or(MapToError::FrameAllocationFailed)?;
            unsafe {
                user_page_mapper
                    .map_to(page, phys_frame, user_flags, frame_allocator)?
                    .flush();
            }
        }

        Ok(())
    }

    pub fn create_main_stack(
        &self,
        pml4_table_phys: PhysAddr,
        stack_size: u64,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<VirtAddr, MapToError<Size4KiB>> {
        let stack_top = VirtAddr::new(USER_STACK_TOP);
        let stack_bottom = stack_top - stack_size;

        let stack_protection_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

        self.map_virt_mem_region(
            pml4_table_phys,
            stack_bottom,
            stack_size,
            stack_protection_flags,
            frame_allocator,
        )?;

        Ok(stack_top)
    }
}
