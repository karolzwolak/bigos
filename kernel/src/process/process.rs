use crate::memory::usermem::USER_STACK_TOP;
use crate::{
    data_structures::vector::Vec,
    process::{
        ElfLoadInfo,
        elf_loader::ElfLoadFlags,
        process_mem::ProcessMemoryLayout,
        shared_state::{SHARED_STATE, SharedState},
    },
    serial_println,
};
use alloc::string::String;
use x86_64::{
    VirtAddr,
    structures::paging::{PageTableFlags, Size4KiB, mapper::MapToError},
};

// Marks terminated children
pub const INVALID_PID: usize = usize::MAX;

pub const MAX_PRIORITY: u8 = 8;
pub const RFLAGS_DEFAULT: u64 = 0x202;
pub const DEFAULT_NEW_PROCESS_STACK_SIZE: u64 = 1 * 1024 * 1024;

pub const PROCESS_USER_VADDR_ALLOC_MAX_SIZE: u64 = 0x0000_0000_5000_0000;
pub const PROCESS_USER_VADDR_ALLOC_START: u64 = 0x0000_0000_1000_0000;
pub const PROCESS_USER_VADDR_ALLOC_END: u64 =
    PROCESS_USER_VADDR_ALLOC_START + PROCESS_USER_VADDR_ALLOC_MAX_SIZE;

pub const PROCESS_WINDOW_BUFFER_BASE: u64 = 0x0000_0001_0000_0000;
// Hard clamp in case, we derive this from framebuffer size normally
pub const PROCESS_WINDOW_BUFFER_HARD_MAX_SIZE: u64 = 64 * 1024 * 1024;

// One page per window, indexed by window id, above the window buffer slots
pub const PROCESS_EVENT_BUFFER_BASE: u64 = 0x0000_0008_0000_0000;

pub const PROCESS_HEAP_VIRT_START: u64 = 0x0000_0000_6000_0000;
pub const PROCESS_HEAP_SIZE_BYTES: u64 = PROCESS_WINDOW_BUFFER_BASE - PROCESS_HEAP_VIRT_START;
pub const PROCESS_HEAP_VIRT_END: u64 = PROCESS_HEAP_VIRT_START + PROCESS_HEAP_SIZE_BYTES;

pub type PID = usize;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProcessState {
    Ready,
    Running,
    Waiting,
    Terminated,
}

#[derive(Debug, Clone, Copy)]
pub struct ProcessResources {
    pub memory_limit: usize,
    pub memory_used: usize,
    pub cpu_time_slice: usize,
}

impl ProcessResources {
    pub fn default() -> Self {
        Self {
            memory_limit: 0,
            memory_used: 0,
            cpu_time_slice: 0,
        }
    }
}

pub const FD_FLAG_READ: u8 = 0x01;
pub const FD_FLAG_WRITE: u8 = 0x02;

#[derive(Debug, Clone, Copy)]
pub struct FileDescriptor {
    pub node_id: usize,
    //pub offset: usize,
    pub flags: u8,
}

impl FileDescriptor {
    pub const INVALID_FD: u64 = u64::MAX;
}

pub struct Process {
    pub pid: PID,
    pub parent_pid: PID,
    pub priority: u8,
    pub state: ProcessState,
    pub name: String,
    pub children: Vec<PID>,
    pub file_descriptors: Vec<FileDescriptor>,

    pub resources: ProcessResources,
    pub exit_code: Option<i32>,
    pub is_out: bool,

    pub memory_layout: ProcessMemoryLayout,
}

unsafe impl Send for Process {}

impl Process {
    pub fn new(
        pid: usize,
        parent_pid: usize,
        priority: u8,
        name: String,
        is_out: bool,
        resources: ProcessResources,
        page_table_base_phys: u64,
    ) -> Result<Self, MapToError<Size4KiB>> {
        let memory_layout =
            ProcessMemoryLayout::with_page_table(x86_64::PhysAddr::new(page_table_base_phys));

        Ok(Self {
            pid,
            parent_pid,
            priority,
            state: ProcessState::Ready,
            name,
            children: Vec::new(),
            file_descriptors: Vec::new(),
            resources,
            exit_code: None,
            is_out,
            memory_layout,
        })
    }

    pub fn create_with_elf(
        elf_info: &ElfLoadInfo,
        name: &str,
        pid: PID,
        parent_pid: PID,
    ) -> Result<Self, MapToError<Size4KiB>> {
        if crate::process::PROCESS_DEBUG_PRINT {
            serial_println!("Process::create_with_elf()");
        }
        //TODO: safer
        let address_space_manager = crate::memory::get_user_mem_mgr();
        let mut frame_allocator = crate::memory::get_frame_allocator();

        let mut memory_layout =
            ProcessMemoryLayout::new(address_space_manager, &mut frame_allocator)?;

        for segment in &elf_info.segments {
            let vaddr = VirtAddr::new(segment.vaddr);
            let in_memory_size = segment.in_memory_size as u64;

            let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
            if segment.flags & ElfLoadFlags::Writable as u32 != 0 {
                flags |= PageTableFlags::WRITABLE;
            }
            if segment.flags & ElfLoadFlags::Executable as u32 == 0 {
                flags |= PageTableFlags::NO_EXECUTE;
            }

            if crate::process::PROCESS_DEBUG_PRINT {
                serial_println!(
                    "  Mapping ELF segment: vaddr={:#x}, size={:#x}, elf_flags={:#x}",
                    vaddr.as_u64(),
                    in_memory_size,
                    segment.flags,
                );
            }

            if let Err(e) = memory_layout.map_owned_region(
                vaddr,
                in_memory_size,
                flags,
                address_space_manager,
                &mut frame_allocator,
            ) {
                memory_layout.free_address_space(address_space_manager, &mut frame_allocator);
                return Err(e);
            }

            // Copy segment file data into the mapped pages via HHDM.
            // translate_user_virt_to_phys returns the physical address of the
            // page containing the given vaddr, so we drive the copy page-by-page.
            let mut bytes_copied: u64 = 0;
            let file_size = segment.in_file_size as u64;
            while bytes_copied < file_size {
                let src_vaddr = vaddr + bytes_copied;
                let phys = address_space_manager
                    .translate_user_virt_to_phys(memory_layout.top_page_table_phys, src_vaddr)
                    .expect("Failed to translate user vaddr for segment copy");
                let hhdm_vaddr = phys.as_u64() + address_space_manager.phys_offset;

                // How many bytes remain in this 4 KiB page?
                let page_offset = src_vaddr.as_u64() & 0xFFF;
                let bytes_in_page = (0x1000 - page_offset).min(file_size - bytes_copied);

                unsafe {
                    core::ptr::copy_nonoverlapping(
                        segment.data.as_ptr().add(bytes_copied as usize),
                        hhdm_vaddr as *mut u8,
                        bytes_in_page as usize,
                    );
                }
                bytes_copied += bytes_in_page;
            }
            if crate::process::PROCESS_DEBUG_PRINT {
                serial_println!("  Copied {} bytes to {:#x}", file_size, vaddr.as_u64());
            }

            // Zero-fill the BSS (in-memory > in-file) via HHDM, page by page.
            if segment.in_memory_size > segment.in_file_size {
                let bss_offset = segment.in_file_size as u64;
                let bss_size = (segment.in_memory_size - segment.in_file_size) as u64;
                let mut bytes_zeroed: u64 = 0;

                while bytes_zeroed < bss_size {
                    let bss_vaddr = vaddr + bss_offset + bytes_zeroed;
                    let phys = address_space_manager
                        .translate_user_virt_to_phys(memory_layout.top_page_table_phys, bss_vaddr)
                        .expect("Failed to translate user vaddr for BSS zeroing");
                    let hhdm_vaddr = phys.as_u64() + address_space_manager.phys_offset;

                    let page_offset = bss_vaddr.as_u64() & 0xFFF;
                    let bytes_in_page = (0x1000 - page_offset).min(bss_size - bytes_zeroed);

                    unsafe {
                        core::ptr::write_bytes(hhdm_vaddr as *mut u8, 0u8, bytes_in_page as usize);
                    }
                    bytes_zeroed += bytes_in_page;
                }
                if crate::process::PROCESS_DEBUG_PRINT {
                    serial_println!(
                        "  Zeroed BSS: {:#x} bytes starting at user vaddr {:#x}",
                        bss_size,
                        (vaddr + bss_offset).as_u64(),
                    );
                }
            }
        }

        let shared_state = SHARED_STATE.get().unwrap().lock();
        shared_state
            .map_into_process(
                address_space_manager,
                &mut memory_layout,
                &mut frame_allocator,
            )
            .unwrap();

        let stack_size = DEFAULT_NEW_PROCESS_STACK_SIZE;
        let stack_top = VirtAddr::new(USER_STACK_TOP);
        let stack_flags =
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        if let Err(e) = memory_layout.map_owned_region(
            stack_top - stack_size,
            stack_size,
            stack_flags,
            address_space_manager,
            &mut frame_allocator,
        ) {
            memory_layout.free_address_space(address_space_manager, &mut frame_allocator);
            return Err(e);
        }

        Ok(Self {
            pid,
            parent_pid: parent_pid,
            priority: 1,
            state: ProcessState::Ready,
            name: String::from(name),
            children: Vec::new(),
            file_descriptors: Vec::new(),
            resources: ProcessResources::default(),
            exit_code: None,
            is_out: true,
            memory_layout,
        })
    }
}
