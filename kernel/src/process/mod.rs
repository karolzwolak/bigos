/// Logs of process creation and teardown, off because every serial byte costs about 87 us
pub const PROCESS_DEBUG_PRINT: bool = false;

pub mod core_pool;
pub mod elf_loader;
pub mod file_syscalls;
pub mod process_manager;
pub mod process_mem;
pub mod process_syscalls;
pub mod scheduler;
pub mod shared_state;
pub mod syscall;
pub mod thread;
pub mod user_access;
pub mod wait;

pub use core_pool::{CORE_POOL, CorePool};
pub use elf_loader::{ElfLoadError, ElfLoadInfo};
pub use process_manager::ProcessManager;

use crate::filesystem::fat32::{FileNodeHandle, INVALID_NODE_HANDLE};
use crate::ipc::channel::{ChannelMapping, ChannelRole, MAX_CHANNELS_PER_PROCESS};
use crate::memory::paging::PAGE_SIZE;
use crate::memory::usermem::USER_STACK_TOP;
use crate::process::syscall::SyscallError;
use crate::process::thread::UserEntry;
use crate::{
    data_structures::vector::Vec,
    process::{
        elf_loader::ElfLoadFlags, process_mem::ProcessMemoryLayout, shared_state::SHARED_STATE,
    },
    serial_println,
};
use alloc::string::String;
use core::cmp::min;
use static_assertions::const_assert;
use x86_64::{
    VirtAddr,
    structures::paging::{PageTableFlags, Size4KiB, mapper::MapToError},
};

// Marks terminated children
pub const INVALID_PID: usize = usize::MAX;

pub const RFLAGS_DEFAULT: u64 = 0x202;
pub const DEFAULT_NEW_PROCESS_STACK_SIZE: u64 = 1024 * 1024;

pub const PROCESS_USER_VADDR_ALLOC_MAX_SIZE: u64 = 0x0000_0000_5000_0000;
pub const PROCESS_USER_VADDR_ALLOC_START: u64 = 0x0000_0000_1000_0000;
pub const PROCESS_USER_VADDR_ALLOC_END: u64 =
    PROCESS_USER_VADDR_ALLOC_START + PROCESS_USER_VADDR_ALLOC_MAX_SIZE;

pub const PROCESS_WINDOW_BUFFER_BASE: u64 = 0x0000_0001_0000_0000;
// Hard clamp in case, we derive this from framebuffer size normally
pub const PROCESS_WINDOW_BUFFER_HARD_MAX_SIZE: u64 = 64 * 1024 * 1024;

// One page per window, indexed by window id, above the window buffer slots
pub const PROCESS_EVENT_BUFFER_BASE: u64 = 0x0000_0008_0000_0000;

// The stdout channel of a process started with REDIRECT_STDOUT: header page, then both data
// views. Above the event buffer pages, which stay below it for any window id under 2^20
pub const PROCESS_STDOUT_CHANNEL_BASE: u64 = 0x0000_0009_0000_0000;

pub const PROCESS_HEAP_VIRT_START: u64 = 0x0000_0000_6000_0000;
pub const PROCESS_HEAP_SIZE_BYTES: u64 = PROCESS_WINDOW_BUFFER_BASE - PROCESS_HEAP_VIRT_START;
pub const PROCESS_HEAP_VIRT_END: u64 = PROCESS_HEAP_VIRT_START + PROCESS_HEAP_SIZE_BYTES;

pub type PID = usize;

/// Limits of the arguments a new process gets, also checked by CreateProcess
pub const MAX_ARGC: usize = 64;
pub const MAX_ARGV_BYTES: usize = 4096;
pub const MAX_PROCESS_NAME_LEN: usize = 32;

const USER_STACK_ALIGNMENT: u64 = 16;
const ARG_SLICE_SIZE_BYTES: usize = core::mem::size_of::<ArgSlice>();

/// One argument as the new process sees it, the string excludes its NUL
/// Layout shared with rustspace `ArgSlice`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ArgSlice {
    pub ptr: u64,
    pub len: u64,
}

impl ArgSlice {
    fn as_bytes(&self) -> [u8; ARG_SLICE_SIZE_BYTES] {
        let mut bytes = [0u8; ARG_SLICE_SIZE_BYTES];
        bytes[..8].copy_from_slice(&self.ptr.to_le_bytes());
        bytes[8..].copy_from_slice(&self.len.to_le_bytes());
        bytes
    }
}

const STARTUP_INFO_SIZE_BYTES: usize = core::mem::size_of::<ProcessStartupInfo>();

/// Per process values the kernel leaves at the very top of the user stack above the arguments.
/// Layout shared with rustspace `ProcessStartupInfo`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessStartupInfo {
    /// PROCESS_STDOUT_CHANNEL_BASE when the process writes its stdout into a channel, 0 otherwise
    pub stdout_header_vaddr: u64,
    pub stdout_data_size_bytes: u64,
}

impl ProcessStartupInfo {
    fn as_bytes(&self) -> [u8; STARTUP_INFO_SIZE_BYTES] {
        let mut bytes = [0u8; STARTUP_INFO_SIZE_BYTES];
        bytes[..8].copy_from_slice(&self.stdout_header_vaddr.to_le_bytes());
        bytes[8..].copy_from_slice(&self.stdout_data_size_bytes.to_le_bytes());
        bytes
    }
}

pub const PROCESS_STARTUP_INFO_VADDR: u64 = USER_STACK_TOP - STARTUP_INFO_SIZE_BYTES as u64;
const_assert!(PROCESS_STARTUP_INFO_VADDR.is_multiple_of(USER_STACK_ALIGNMENT));

/// What happens to the children of a terminating process
/// The raw values are the syscall ABI, 0 is the default so callers that pass nothing kill the subtree
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildPolicy {
    /// every child is killed with the same exit code, and so on down the subtree
    KillChildren = 0,
    /// the children keep running and bigproc (PID 1) adopts them
    OrphanChildren = 1,
}

impl ChildPolicy {
    /// For values the kernel stored itself
    pub const fn from_raw(raw: u8) -> Self {
        debug_assert!(raw <= ChildPolicy::OrphanChildren as u8);

        match raw {
            0 => ChildPolicy::KillChildren,
            _ => ChildPolicy::OrphanChildren,
        }
    }
}

impl TryFrom<u64> for ChildPolicy {
    type Error = SyscallError;

    /// For values coming from userspace
    fn try_from(raw: u64) -> Result<Self, Self::Error> {
        match raw {
            0 => Ok(ChildPolicy::KillChildren),
            1 => Ok(ChildPolicy::OrphanChildren),
            _ => Err(SyscallError::InvalidArgument),
        }
    }
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

pub const FD_FLAG_READ: u8 = 0x01;
pub const FD_FLAG_WRITE: u8 = 0x02;
pub const FD_FLAGS_VALID_MASK: u8 = FD_FLAG_READ | FD_FLAG_WRITE;
pub const MAX_OPEN_FILES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDescriptor {
    pub node_id: FileNodeHandle,
    pub flags: u8,
}

impl FileDescriptor {
    /// Placeholder of a free slot, no real file has this node id
    pub const CLOSED: Self = Self {
        node_id: INVALID_NODE_HANDLE,
        flags: 0,
    };

    pub const fn is_open(&self) -> bool {
        self.node_id != INVALID_NODE_HANDLE
    }
}

pub struct Process {
    pub pid: PID,
    pub parent_pid: PID,
    pub priority: u8,
    pub state: ProcessState,
    pub name: String,
    pub children: Vec<PID>,
    pub file_descriptors: [FileDescriptor; MAX_OPEN_FILES],
    /// Channels mapped into this process, free slots hold ChannelMapping::NONE
    pub channels: [ChannelMapping; MAX_CHANNELS_PER_PROCESS],
    /// Write syscalls on fd 1 and 2, prints that go straight into a channel never count
    pub stdout_write_syscall_count: u32,

    pub resources: ProcessResources,
    pub exit_code: Option<i32>,
    pub is_out: bool,

    pub memory_layout: ProcessMemoryLayout,
}

unsafe impl Send for Process {}

impl Process {
    /// Takes the lowest free slot, so a closed number is reused first
    pub fn open_fd(&mut self, node_id: FileNodeHandle, flags: u8) -> Result<usize, SyscallError> {
        debug_assert!(node_id != INVALID_NODE_HANDLE);
        debug_assert!(flags & !FD_FLAGS_VALID_MASK == 0);

        let Some(fd) = self
            .file_descriptors
            .iter()
            .position(|slot| !slot.is_open())
        else {
            return Err(SyscallError::TooManyOpenFiles);
        };
        self.file_descriptors[fd] = FileDescriptor { node_id, flags };

        Ok(fd)
    }

    pub fn get_fd(&self, fd: usize) -> Result<FileDescriptor, SyscallError> {
        match self.file_descriptors.get(fd) {
            Some(descriptor) if descriptor.is_open() => Ok(*descriptor),
            _ => Err(SyscallError::InvalidFd),
        }
    }

    pub fn close_fd(&mut self, fd: usize) -> Result<(), SyscallError> {
        self.get_fd(fd)?;
        self.file_descriptors[fd] = FileDescriptor::CLOSED;

        Ok(())
    }

    /// Copies bytes into this process, through the HHDM, into pages that are already mapped
    fn write_user_bytes(&self, user_vaddr: u64, bytes: &[u8]) {
        let address_space_manager = crate::memory::get_user_mem_mgr();
        let mut bytes_written = 0;

        while bytes_written < bytes.len() {
            let destination = VirtAddr::new(user_vaddr + bytes_written as u64);
            let phys = address_space_manager
                .translate_user_virt_to_phys(self.memory_layout.top_page_table_phys, destination)
                .expect("argument page is not mapped");
            let hhdm_vaddr = phys.as_u64() + address_space_manager.phys_offset;

            let page_offset = destination.as_u64() as usize % PAGE_SIZE;
            let chunk_len = min(PAGE_SIZE - page_offset, bytes.len() - bytes_written);

            // SAFETY: the page belongs to this process and nothing runs in it yet
            unsafe {
                core::ptr::copy_nonoverlapping(
                    bytes.as_ptr().add(bytes_written),
                    hhdm_vaddr as *mut u8,
                    chunk_len,
                );
            }

            bytes_written += chunk_len;
        }
    }

    /// Puts the ProcessStartupInfo at the top of the user stack, the packed arguments below it
    /// and the ArgSlice array below them. Call it after the stdout channel is attached
    /// Returns the registers of the first entry, rsp points at the array
    pub fn place_arguments(&self, entry_point: u64, argc: u32, packed_args: &[u8]) -> UserEntry {
        let argc = argc as usize;
        debug_assert!(argc <= MAX_ARGC && packed_args.len() <= MAX_ARGV_BYTES);
        debug_assert!(packed_args.iter().filter(|&&byte| byte == 0).count() == argc);

        let stdout_channel = self.stdout_channel();
        let startup_info = if stdout_channel.is_mapped() {
            ProcessStartupInfo {
                stdout_header_vaddr: stdout_channel.header_vaddr,
                stdout_data_size_bytes: stdout_channel.data_size_bytes as u64,
            }
        } else {
            ProcessStartupInfo {
                stdout_header_vaddr: 0,
                stdout_data_size_bytes: 0,
            }
        };
        self.write_user_bytes(PROCESS_STARTUP_INFO_VADDR, &startup_info.as_bytes());

        let strings_base = PROCESS_STARTUP_INFO_VADDR - packed_args.len() as u64;
        let array_bytes = (argc * ARG_SLICE_SIZE_BYTES) as u64;
        let array_base = (strings_base - array_bytes) & !(USER_STACK_ALIGNMENT - 1);

        let mut array = [0u8; MAX_ARGC * ARG_SLICE_SIZE_BYTES];
        let mut string_offset = 0;
        for (index, argument) in packed_args.split(|&byte| byte == 0).take(argc).enumerate() {
            let slice = ArgSlice {
                ptr: strings_base + string_offset as u64,
                len: argument.len() as u64,
            };
            let array_offset = index * ARG_SLICE_SIZE_BYTES;
            array[array_offset..array_offset + ARG_SLICE_SIZE_BYTES]
                .copy_from_slice(&slice.as_bytes());

            string_offset += argument.len() + 1;
        }

        self.write_user_bytes(strings_base, packed_args);
        self.write_user_bytes(array_base, &array[..argc * ARG_SLICE_SIZE_BYTES]);

        UserEntry {
            rip: entry_point,
            rsp: array_base,
            argc: argc as u64,
            argv: array_base,
        }
    }

    /// The channel this process writes its stdout into, NONE when it has none
    pub fn stdout_channel(&self) -> ChannelMapping {
        self.channels
            .iter()
            .find(|mapping| mapping.is_mapped() && mapping.role == ChannelRole::Writer)
            .copied()
            .unwrap_or(ChannelMapping::NONE)
    }

    pub fn close_all_fds(&mut self) {
        self.file_descriptors = [FileDescriptor::CLOSED; MAX_OPEN_FILES];
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
            let in_memory_size = segment.in_memory_size;

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
            let file_size = segment.in_file_size;
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
                let bss_offset = segment.in_file_size;
                let bss_size = segment.in_memory_size - segment.in_file_size;
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
            parent_pid,
            priority: 1,
            state: ProcessState::Ready,
            name: String::from(name),
            children: Vec::new(),
            file_descriptors: [FileDescriptor::CLOSED; MAX_OPEN_FILES],
            channels: [ChannelMapping::NONE; MAX_CHANNELS_PER_PROCESS],
            stdout_write_syscall_count: 0,
            resources: ProcessResources::default(),
            exit_code: None,
            is_out: true,
            memory_layout,
        })
    }
}
