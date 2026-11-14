use crate::ipc::channel::{ChannelId, NO_CHANNEL};
use crate::process::elf_loader::load_elf_from_path;
use crate::process::process_manager::{BIGPROC_PID, PROCESS_MANAGER, ProcessSpawn};
use crate::process::scheduler::{self, MAX_PRIORITY};
use crate::process::syscall::{SyscallError, SyscallFrame};
use crate::process::user_access::{UserAccess, copy_from_user, copy_user_path, validate_user_ptr};
use crate::process::{
    FileDescriptor, MAX_ARGC, MAX_ARGV_BYTES, MAX_OPEN_FILES, MAX_PROCESS_NAME_LEN, Process,
    ProcessState,
};
use crate::util::cpuinfo::get_current_core_id;
use bitflags::bitflags;
use static_assertions::const_assert;

const ARGUMENT_TERMINATOR: u8 = 0;
const RESERVED_BYTES: usize = 5;

/// Layout shared with rustspace `ProcessCreateInfo`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessCreateInfo {
    pub path_ptr: u64,
    pub path_len: u64,
    pub name_ptr: u64,
    pub name_len: u64,
    pub argv_ptr: u64,
    pub argv_size: u64,
    pub argc: u32,
    /// ProcessCreateFlags bits
    pub flags: u32,
    /// ignored with INHERIT_PRIORITY
    pub priority: u8,
    pub reserved: [u8; RESERVED_BYTES],
    /// read only with REDIRECT_STDOUT, must be 0 without it
    pub stdout_channel: ChannelId,
}

const PROCESS_CREATE_INFO_SIZE_BYTES: usize = 64;
const_assert!(core::mem::size_of::<ProcessCreateInfo>() == PROCESS_CREATE_INFO_SIZE_BYTES);
const_assert!(core::mem::align_of::<ProcessCreateInfo>() == core::mem::align_of::<u64>());

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ProcessCreateFlags: u32 {
        /// the child starts with a copy of the caller's fd table
        const INHERIT_FDS = 1 << 0;
        /// the child is parented to bigproc
        const DETACHED = 1 << 1;
        /// the child gets the caller's priority, the priority field is ignored
        const INHERIT_PRIORITY = 1 << 2;
        /// stdout and stderr of the child go into stdout_channel, the caller must be its reader
        const REDIRECT_STDOUT = 1 << 3;
    }
}

fn copy_create_info(info_ptr: usize) -> Result<ProcessCreateInfo, SyscallError> {
    let mut bytes = [0u8; PROCESS_CREATE_INFO_SIZE_BYTES];
    copy_from_user(info_ptr, &mut bytes)?;

    // SAFETY: every bit pattern is a valid ProcessCreateInfo, read_unaligned needs no alignment
    Ok(unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const ProcessCreateInfo) })
}

/// argc NUL-terminated UTF-8 strings that fill the buffer exactly
fn validate_packed_args(packed_args: &[u8], argc: usize) -> Result<(), SyscallError> {
    if argc == 0 {
        return if packed_args.is_empty() {
            Ok(())
        } else {
            Err(SyscallError::InvalidArgument)
        };
    }
    if packed_args.last() != Some(&ARGUMENT_TERMINATOR) {
        return Err(SyscallError::InvalidArgument);
    }

    let terminator_count = packed_args
        .iter()
        .filter(|&&byte| byte == ARGUMENT_TERMINATOR)
        .count();
    if terminator_count != argc {
        return Err(SyscallError::InvalidArgument);
    }

    let without_last_terminator = &packed_args[..packed_args.len() - 1];
    for argument in without_last_terminator.split(|&byte| byte == ARGUMENT_TERMINATOR) {
        if core::str::from_utf8(argument).is_err() {
            return Err(SyscallError::InvalidArgument);
        }
    }

    Ok(())
}

/// CreateProcess(info_ptr) -> pid of the child
pub fn create_process(frame: &SyscallFrame) -> Result<u64, SyscallError> {
    let info = copy_create_info(frame.arg1 as usize)?;

    let flags = ProcessCreateFlags::from_bits(info.flags).ok_or(SyscallError::InvalidArgument)?;
    if info.reserved != [0; RESERVED_BYTES] {
        return Err(SyscallError::InvalidArgument);
    }
    let stdout_channel = if flags.contains(ProcessCreateFlags::REDIRECT_STDOUT) {
        if info.stdout_channel == NO_CHANNEL {
            return Err(SyscallError::InvalidArgument);
        }
        info.stdout_channel
    } else {
        if info.stdout_channel != 0 {
            return Err(SyscallError::InvalidArgument);
        }
        NO_CHANNEL
    };
    if !flags.contains(ProcessCreateFlags::INHERIT_PRIORITY)
        && info.priority as usize > MAX_PRIORITY
    {
        return Err(SyscallError::InvalidArgument);
    }

    let path = copy_user_path(info.path_ptr as usize, info.path_len as usize)?;

    let name_len = info.name_len as usize;
    if name_len > MAX_PROCESS_NAME_LEN {
        return Err(SyscallError::InvalidArgument);
    }
    let mut name_bytes = [0u8; MAX_PROCESS_NAME_LEN];
    copy_from_user(info.name_ptr as usize, &mut name_bytes[..name_len])?;
    let name = if name_len == 0 {
        path.as_str().rsplit('/').next().unwrap_or(path.as_str())
    } else {
        core::str::from_utf8(&name_bytes[..name_len]).map_err(|_| SyscallError::InvalidArgument)?
    };
    // a path ending in '/' leaves no file name to default to
    if name.is_empty() || name.len() > MAX_PROCESS_NAME_LEN {
        return Err(SyscallError::InvalidArgument);
    }

    let argc = info.argc as usize;
    let argv_size = info.argv_size as usize;
    if argc > MAX_ARGC || argv_size > MAX_ARGV_BYTES {
        return Err(SyscallError::InvalidArgument);
    }
    let mut packed_args = [0u8; MAX_ARGV_BYTES];
    let packed_args = &mut packed_args[..argv_size];
    copy_from_user(info.argv_ptr as usize, packed_args)?;
    validate_packed_args(packed_args, argc)?;

    let caller_pid = scheduler::get_current_process_for_core(get_current_core_id());

    // takes and releases the VFS lock, PROCESS_MANAGER is only taken afterwards
    let elf_info = load_elf_from_path(path.as_str())?;

    let mut process_manager = PROCESS_MANAGER.lock();
    let caller = process_manager.get_process(caller_pid)?;

    let priority = if flags.contains(ProcessCreateFlags::INHERIT_PRIORITY) {
        caller.priority
    } else {
        info.priority
    };
    let file_descriptors = if flags.contains(ProcessCreateFlags::INHERIT_FDS) {
        caller.file_descriptors
    } else {
        [FileDescriptor::CLOSED; MAX_OPEN_FILES]
    };
    let parent_pid = if flags.contains(ProcessCreateFlags::DETACHED) {
        BIGPROC_PID
    } else {
        caller_pid
    };

    let spawn = ProcessSpawn {
        parent_pid,
        name,
        priority,
        argc: info.argc,
        packed_args,
        file_descriptors,
        spawning_pid: caller_pid,
        stdout_channel,
    };
    let pid = process_manager.spawn(&spawn, &elf_info)?;

    Ok(pid as u64)
}

const NAME_RESERVED_BYTES: usize = 7;

/// One ListProcesses entry, layout shared with rustspace `ProcessInfoFlat`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessInfoFlat {
    pub pid: u64,
    pub name_len: u8,
    pub reserved: [u8; NAME_RESERVED_BYTES],
    /// not NUL-terminated, name_len bytes are valid
    pub name: [u8; MAX_PROCESS_NAME_LEN],
}

const PROCESS_INFO_FLAT_SIZE_BYTES: usize = core::mem::size_of::<ProcessInfoFlat>();
const_assert!(PROCESS_INFO_FLAT_SIZE_BYTES == 48);

impl ProcessInfoFlat {
    fn from_process(process: &Process) -> Self {
        debug_assert!(process.name.len() <= MAX_PROCESS_NAME_LEN);

        let name_len = process.name.len();
        let mut entry = Self {
            pid: process.pid as u64,
            name_len: name_len as u8,
            reserved: [0; NAME_RESERVED_BYTES],
            name: [0; MAX_PROCESS_NAME_LEN],
        };
        entry.name[..name_len].copy_from_slice(process.name.as_bytes());
        entry
    }
}

/// ListProcesses(out_ptr, out_size_bytes, start_index) -> entries written
/// Lists the processes that have not terminated yet, start_index pages through them
pub fn list_processes(frame: &SyscallFrame) -> Result<u64, SyscallError> {
    let out_ptr = frame.arg1 as usize;
    let capacity = frame.arg2 as usize / PROCESS_INFO_FLAT_SIZE_BYTES;
    let start_index = frame.arg3 as usize;
    if !validate_user_ptr(
        out_ptr,
        capacity * PROCESS_INFO_FLAT_SIZE_BYTES,
        UserAccess::Write,
    ) {
        return Err(SyscallError::InvalidPtr);
    }

    let process_manager = PROCESS_MANAGER.lock();
    let page = process_manager
        .get_process_list()
        .iter()
        .filter(|process| process.state != ProcessState::Terminated)
        .skip(start_index)
        .take(capacity);

    let mut written = 0;
    for process in page {
        // SAFETY: slot written is below capacity, inside the validated range
        unsafe {
            (out_ptr as *mut ProcessInfoFlat)
                .byte_add(written * PROCESS_INFO_FLAT_SIZE_BYTES)
                .write_unaligned(ProcessInfoFlat::from_process(process));
        }
        written += 1;
    }

    Ok(written as u64)
}
