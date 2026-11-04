use crate::memory::memory::MemoryMapFrameAllocator;
use crate::serial_println;
use x86_64::structures::paging::FrameAllocator;
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{Page, PageTable, PageTableFlags, PhysFrame, Size4KiB},
};

const PAGE_SIZE: u64 = 4096;

pub unsafe fn unmap_guard_page(
    vaddr: VirtAddr,
    frame_allocator: &mut MemoryMapFrameAllocator,
) -> Result<(), &'static str> {
    let hhdm = crate::boot_info::boot_info().hhdm_offset;
    let (pml4_frame, _) = Cr3::read();
    let pml4_virt = VirtAddr::new(pml4_frame.start_address().as_u64() + hhdm);
    let pml4 = unsafe { &mut *pml4_virt.as_mut_ptr::<PageTable>() };

    let p4_idx = ((vaddr.as_u64() >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((vaddr.as_u64() >> 30) & 0x1FF) as usize;
    let pd_idx = ((vaddr.as_u64() >> 21) & 0x1FF) as usize;
    let pt_idx = ((vaddr.as_u64() >> 12) & 0x1FF) as usize;

    let p4_entry = &mut pml4[p4_idx];
    if p4_entry.is_unused() {
        return Err("p4 entry unused");
    }
    let pdpt_phys = p4_entry.addr();
    let pdpt_virt = VirtAddr::new(pdpt_phys.as_u64() + hhdm);
    let pdpt = unsafe { &mut *pdpt_virt.as_mut_ptr::<PageTable>() };

    let pdpt_entry = &mut pdpt[pdpt_idx];
    if pdpt_entry.is_unused() {
        return Err("pdpt entry unused");
    }
    // check for 1GiB huge page at PDPT level
    if pdpt_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
        return Err("pdpt huge page not supported for guard");
    }
    let pd_phys = pdpt_entry.addr();
    let pd_virt = VirtAddr::new(pd_phys.as_u64() + hhdm);
    let pd = unsafe { &mut *pd_virt.as_mut_ptr::<PageTable>() };

    let pd_entry = &mut pd[pd_idx];
    if pd_entry.is_unused() {
        return Err("pd entry unused");
    }

    if pd_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
        // 2MiB huge page -> split into 512 4KiB pages
        let huge_phys_base = pd_entry.addr().as_u64();
        let huge_flags = pd_entry.flags();
        // allocate new PT
        let pt_frame = frame_allocator
            .allocate_frame()
            .ok_or("out of frames for PT split")?;
        let pt_phys = pt_frame.start_address();
        let pt_virt = VirtAddr::new(pt_phys.as_u64() + hhdm);
        let pt = unsafe { &mut *pt_virt.as_mut_ptr::<PageTable>() };
        // zero PT
        for entry in pt.iter_mut() {
            entry.set_unused();
        }

        let base_flags = huge_flags & !PageTableFlags::HUGE_PAGE;
        for i in 0..512 {
            let cur_vaddr = VirtAddr::new((huge_phys_base & !0x1FFFFF) + i as u64 * PAGE_SIZE);
            let phys = PhysAddr::new(huge_phys_base + i as u64 * PAGE_SIZE);
            // ensure present
            let mut flags = base_flags;
            flags.insert(PageTableFlags::PRESENT);
            pt[i].set_addr(phys, flags);
        }
        // guard page -> not present
        pt[pt_idx].set_unused();

        // update PD entry to point to new PT
        let mut pd_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        pd_entry.set_addr(pt_phys, pd_flags);

        unsafe {
            x86_64::instructions::tlb::flush_all();
        }

        serial_println!(
            "guard: split 2MiB huge page at pd_idx {} for vaddr {:#x}",
            pd_idx,
            vaddr.as_u64()
        );

        return Ok(());
    } else {
        // 4KiB mapping via PT
        let pt_phys = pd_entry.addr();
        let pt_virt = VirtAddr::new(pt_phys.as_u64() + hhdm);
        let pt = unsafe { &mut *pt_virt.as_mut_ptr::<PageTable>() };
        let pt_entry = &mut pt[pt_idx];
        if pt_entry.is_unused() {
            return Err("pt entry already unused (guard already unmapped?)");
        }

        pt_entry.set_unused();

        unsafe {
            x86_64::instructions::tlb::flush(VirtAddr::new(vaddr.as_u64()));
        }

        return Ok(());
    }
}

pub unsafe fn ensure_guard_unmapped(
    vaddr: VirtAddr,
    frame_allocator: &mut MemoryMapFrameAllocator,
) {
    match unsafe { unmap_guard_page(vaddr, frame_allocator) } {
        Ok(()) => serial_println!("guard: unmapped {:#x} ok", vaddr.as_u64()),
        Err(e) => serial_println!("guard: failed to unmap {:#x}: {}", vaddr.as_u64(), e),
    }
}
