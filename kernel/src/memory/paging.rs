use crate::serial_println;
use acpi::{Handler, PhysicalMapping};
use limine::memory_map::{Entry, EntryType};
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB, mapper::MapToError,
    },
};

pub const PAGE_SIZE: usize = 4096; // 4 KiB

#[derive(Clone, Copy)]
pub struct IdendtityAcpiHandler {
    pub phys_offset: u64,
}

/// # Safety
///
/// `hhdm_offset` must be the correct higher-half direct mapping offset provided by the bootloader.
/// The CR3 register must contain a valid, correctly-mapped level-4 page table accessible at
/// `hhdm_offset + phys_addr`.
pub unsafe fn init_offset_page_table(hhdm_offset: u64) -> OffsetPageTable<'static> {
    let phys_mem_offset = VirtAddr::new(hhdm_offset);
    serial_println!(
        "Initializing offset page table with physical memory offset: {:#x}",
        phys_mem_offset
    );

    let (l4_table_frame, _flags) = Cr3::read();
    let phys_addr = l4_table_frame.start_address();
    let virt_addr = phys_mem_offset + phys_addr.as_u64();
    let page_table_ptr: *mut PageTable = virt_addr.as_mut_ptr();

    unsafe {
        let l4_table = &mut *page_table_ptr;
        OffsetPageTable::new(l4_table, phys_mem_offset)
    }
}

/// iterates through USABLE memory regions and hands out 4KB physical frames on demand
pub struct MemoryMapFrameAllocator {
    memory_map: &'static [&'static Entry],
    curr_region_index: usize,
    frame_offset_in_region: u64,
}

pub const fn align_up(x: u64, align: u64) -> u64 {
    (x + align - 1) & !(align - 1)
}

pub const fn align_down(x: u64, align: u64) -> u64 {
    x & !(align - 1)
}

pub fn map_acpi_regions(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut MemoryMapFrameAllocator,
    rsdp_phys_addr: usize,
    hhdm_offset: u64,
) -> Result<(), MapToError<Size4KiB>> {
    serial_println!("Mapping ACPI regions");

    // map the RSDP page
    let rsdp_start = align_down(rsdp_phys_addr as u64, PAGE_SIZE as u64);
    let rsdp_end = rsdp_start + PAGE_SIZE as u64;
    map_region(
        mapper,
        frame_allocator,
        rsdp_start,
        rsdp_end,
        hhdm_offset,
        true,
    )?;

    // map all ACPI regions
    for entry in frame_allocator.memory_map {
        if entry.entry_type == EntryType::ACPI_RECLAIMABLE
            || entry.entry_type == EntryType::ACPI_NVS
        {
            let start = align_down(entry.base, PAGE_SIZE as u64);
            let end = align_up(entry.base + entry.length, PAGE_SIZE as u64);

            map_region(mapper, frame_allocator, start, end, hhdm_offset, true)?;
        }
    }

    Ok(())
}

fn map_region(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    phys_start: u64,
    phys_end: u64,
    hhdm_offset: u64,
    cached_pages: bool,
) -> Result<(), MapToError<Size4KiB>> {
    debug_assert!(phys_end > phys_start);

    let mut phys = phys_start;
    let flags = if cached_pages {
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE
    } else {
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE
    };

    while phys < phys_end {
        let frame = PhysFrame::containing_address(PhysAddr::new(phys));
        let virt = VirtAddr::new(phys + hhdm_offset);
        let page = Page::containing_address(virt);

        unsafe {
            match mapper.map_to(page, frame, flags, frame_allocator) {
                Ok(mapper_flush) => {
                    mapper_flush.flush();
                }
                Err(e) => match e {
                    MapToError::PageAlreadyMapped(_) => {
                        serial_println!(
                            "Error: Mapping page phys {:#x} -> virt {:#x} --> already mapped!",
                            phys,
                            virt.as_u64()
                        );
                    }
                    _ => {
                        serial_println!(
                            "Error: Mapping page phys {:#x} -> virt {:#x} - failed!",
                            phys,
                            virt.as_u64()
                        );
                        return Err(e);
                    }
                },
            }
        }

        phys += PAGE_SIZE as u64;
    }

    Ok(())
}

impl MemoryMapFrameAllocator {
    /// # Safety
    ///
    /// `memory_map` must be valid for the `'static` lifetime and accurately describe all usable
    /// physical memory regions. Calling this with an incorrect map may cause the allocator to hand
    /// out frames that overlap with firmware or kernel data.
    pub unsafe fn init(memory_map: &'static [&'static Entry]) -> Self {
        serial_println!("Initializing frame allocator with memory map:");

        for entry in memory_map {
            serial_println!("Base: {:#x}, Length: {:#x}", entry.base, entry.length,);
        }

        Self {
            memory_map,
            curr_region_index: 0,
            frame_offset_in_region: 0,
        }
    }

    fn _usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        let regions = self.memory_map.iter();
        let usable_regions = regions.filter(|r| r.entry_type == EntryType::USABLE);
        let addr_ranges = usable_regions.map(|r| r.base..r.base + r.length);
        let frame_addresses = addr_ranges.flat_map(|r| r.step_by(PAGE_SIZE));

        frame_addresses.map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

unsafe impl FrameAllocator<Size4KiB> for MemoryMapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        loop {
            let region = self.memory_map.get(self.curr_region_index)?;

            if region.entry_type != EntryType::USABLE {
                self.curr_region_index += 1;
                self.frame_offset_in_region = 0;
                continue;
            }

            let page_size: u64 = PAGE_SIZE as u64;
            let start = align_up(region.base, page_size);
            let end = region.base + region.length;

            let addr = start + self.frame_offset_in_region;

            if addr + page_size <= end {
                self.frame_offset_in_region += page_size;
                return Some(PhysFrame::containing_address(PhysAddr::new(addr)));
            } else {
                self.curr_region_index += 1;
                self.frame_offset_in_region = 0;
            }
        }
    }
}

impl Handler for IdendtityAcpiHandler {
    unsafe fn map_physical_region<T>(
        &self,
        phys_addr: usize,
        size: usize,
    ) -> PhysicalMapping<Self, T> {
        let virt_addr = (phys_addr as u64 + self.phys_offset) as *mut T;

        PhysicalMapping {
            physical_start: phys_addr,
            virtual_start: unsafe { core::ptr::NonNull::new_unchecked(virt_addr) },
            region_length: size,
            mapped_length: size,
            handler: *self,
        }
    }

    fn unmap_physical_region<T>(_region: &PhysicalMapping<Self, T>) {
        serial_println!("Unmapping physical region");
    }

    fn read_u8(&self, __address: usize) -> u8 {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn read_u16(&self, __address: usize) -> u16 {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn read_u32(&self, __address: usize) -> u32 {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn read_u64(&self, __address: usize) -> u64 {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn write_u8(&self, __address: usize, __value: u8) {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn write_u16(&self, __address: usize, __value: u16) {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn write_u32(&self, __address: usize, _value: u32) {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn write_u64(&self, __address: usize, _value: u64) {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn read_io_u8(&self, _port: u16) -> u8 {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn read_io_u16(&self, _port: u16) -> u16 {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn read_io_u32(&self, _port: u16) -> u32 {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn write_io_u8(&self, _port: u16, __value: u8) {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn write_io_u16(&self, _port: u16, __value: u16) {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn write_io_u32(&self, _port: u16, _value: u32) {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn read_pci_u8(&self, _address: acpi::PciAddress, _offset: u16) -> u8 {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn read_pci_u16(&self, _address: acpi::PciAddress, _offset: u16) -> u16 {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn read_pci_u32(&self, _address: acpi::PciAddress, _offset: u16) -> u32 {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn write_pci_u8(&self, _address: acpi::PciAddress, _offset: u16, __value: u8) {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn write_pci_u16(&self, _address: acpi::PciAddress, _offset: u16, __value: u16) {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn write_pci_u32(&self, _address: acpi::PciAddress, _offset: u16, _value: u32) {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn nanos_since_boot(&self) -> u64 {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn stall(&self, _microseconds: u64) {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn sleep(&self, _milliseconds: u64) {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn create_mutex(&self) -> acpi::Handle {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn acquire(&self, _mutex: acpi::Handle, _timeout: u16) -> Result<(), acpi::aml::AmlError> {
        //AcpiError::HostUnimplemented
        todo!()
    }

    fn release(&self, _mutex: acpi::Handle) {
        //AcpiError::HostUnimplemented
        todo!()
    }
}
