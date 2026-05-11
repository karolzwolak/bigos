use crate::{
    data_structures::vector::Vec,
    process::{elf_loader::ElfLoadInfo, process_mem::ProcessMemoryLayout},
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
pub const DEFAULT_NEW_PROCESS_STACK_SIZE: u64 = 1024 * 1024;

pub type PID = usize;

//TODO: temporary until no scheduler
static CURRENT_PROCESS: spin::Once<spin::Mutex<Option<Process>>> = spin::Once::new();

pub fn set_current_process(process: Process) {
    CURRENT_PROCESS.call_once(|| spin::Mutex::new(Some(process)));
}

pub fn get_current_process() -> &'static spin::Mutex<Option<Process>> {
    CURRENT_PROCESS
        .get()
        .expect("Current process not initialized")
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProcessState {
    Ready,
    Running,
    Waiting,
    Terminated,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessResources {
    pub memory_limit: usize,
    pub memory_used: usize,
    pub cpu_time_slice: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ExecutionContext {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,

    //TODO: sse? maybe somewhere seperate because aint storing the whole avx
    pub rip: u64,
    pub rflags: u64,

    pub page_table_base_phys: u64,
}

//TODO: when we have a fs/vfs
pub struct FileDescriptor {
    pub handle: usize,
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

    pub execution_context: ExecutionContext,
    pub memory_layout: ProcessMemoryLayout,
}

unsafe impl Send for Process {}

impl ExecutionContext {
    pub fn new(entry_point: u64, stack_top: u64, page_table_base_phys: u64) -> Self {
        Self {
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            rbp: 0,
            rsp: stack_top,
            r8: 0,
            r9: 0,
            r10: 0,
            r11: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rip: entry_point,
            rflags: RFLAGS_DEFAULT,
            page_table_base_phys,
        }
    }
}

impl Process {
    // pub fn new(
    //     pid: usize,
    //     parent_pid: usize,
    //     priority: u8,
    //     name: String,
    //     is_out: bool,
    //     resources: ProcessResources,
    //     entry_point: u64,
    //     stack_top: u64,
    //     page_table_base_phys: u64,
    // ) -> Self {
    //     Self {
    //         pid,
    //         parent_pid,
    //         priority,
    //         state: ProcessState::Ready,
    //         name,
    //         children: Vec::new(),
    //         file_descriptors: Vec::new(),
    //         resources,
    //         exit_code: None,
    //         is_out,
    //         execution_context: ExecutionContext::new(entry_point, stack_top, page_table_base_phys),
    //         memory_layout: ProcessMemoryLayout::empty()
    //     }
    // }

    pub fn create_with_elf(
        elf_info: &ElfLoadInfo,
        name: &str,
        pid: PID,
        parent_pid: PID,
    ) -> Result<Self, MapToError<Size4KiB>> {
        serial_println!("Process::create_with_elf()");
        //TODO: safer
        let address_space_manager = &crate::memory::get_user_mem_mgr();
        let mut frame_allocator = crate::memory::get_frame_allocator();

        let mut memory_layout =
            ProcessMemoryLayout::new(address_space_manager, &mut frame_allocator)?;

        for segment in &elf_info.segments {
            let vaddr = VirtAddr::new(segment.vaddr);
            let in_memory_size = segment.in_memory_size;

            // TODO: Parse actual segment flags from ELF
            let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
            flags |= PageTableFlags::WRITABLE;

            serial_println!(
                "  Mapping ELF segment: vaddr={:#x}, size={:#x}",
                vaddr.as_u64(),
                in_memory_size
            );

            address_space_manager.map_virt_mem_region(
                memory_layout.top_page_table_phys,
                vaddr,
                in_memory_size,
                flags,
                &mut frame_allocator,
            )?;

            let phys_addr = address_space_manager
                .translate_user_virt_to_phys(memory_layout.top_page_table_phys, vaddr)
                .expect("Failed to translate user virtual address to physical");
            let hhdm_vaddr = phys_addr.as_u64() + address_space_manager.phys_offset;

            // copy segment data
            unsafe {
                core::ptr::copy_nonoverlapping(
                    segment.data.as_ptr(),
                    hhdm_vaddr as *mut u8,
                    segment.in_file_size as usize,
                );
            }
            serial_println!(
                "  Copied {} bytes to {:#x}",
                segment.in_file_size,
                vaddr.as_u64()
            );

            // zero-fill the bss section
            if segment.in_memory_size > segment.in_file_size {
                let bss_start = vaddr + segment.in_file_size;
                let bss_size = segment.in_memory_size - segment.in_file_size;
                unsafe {
                    core::ptr::write_bytes(bss_start.as_mut_ptr::<u8>(), 0u8, bss_size as usize);
                }
                serial_println!(
                    "  Zeroed BSS: {:#x} bytes at {:#x}",
                    bss_size,
                    bss_start.as_u64()
                );
            }

            // track mapped region
            memory_layout
                .mapped_regions
                .push(crate::process::process_mem::MappedMemoryRegion {
                    start_virt: vaddr,
                    size_bytes: in_memory_size,
                    page_flags: flags,
                });
        }

        let stack_size = DEFAULT_NEW_PROCESS_STACK_SIZE;
        let stack_top = address_space_manager.create_main_stack(
            memory_layout.top_page_table_phys,
            stack_size,
            &mut frame_allocator,
        )?;
        memory_layout.stack_top = stack_top;

        let context = ExecutionContext::new(
            elf_info.entry_point,
            stack_top.as_u64(),
            memory_layout.top_page_table_phys.as_u64(),
        );

        Ok(Self {
            pid,
            parent_pid,
            priority: 1,
            state: ProcessState::Ready,
            name: String::from(name),
            children: Vec::new(),
            file_descriptors: Vec::new(),
            resources: ProcessResources::default(),
            exit_code: None,
            is_out: true,
            execution_context: context,
            memory_layout,
        })
    }
}
