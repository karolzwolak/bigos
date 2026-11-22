use crate::filesystem::fat32::direntry::{MAX_EXT_LENGTH, MAX_NAME_LENGTH};
use crate::filesystem::vfs::FileAttributes;
use crate::filesystem::{FileNode, FileType, get_vfs};
use crate::process::process_manager::PROCESS_MANAGER;
use crate::process::syscall::{SyscallError, SyscallFrame};
use crate::process::user_access::{UserAccess, copy_user_path, validate_user_ptr};
use crate::process::{
    FD_FLAG_READ, FD_FLAG_WRITE, FD_FLAGS_VALID_MASK, FileDescriptor, PID, scheduler,
};
use crate::util::cpuinfo::get_current_core_id;
use static_assertions::const_assert;

pub const FS_NAME_LEN: usize = 16;

// 8.3 name plus the dot
const_assert!(MAX_NAME_LENGTH + 1 + MAX_EXT_LENGTH <= FS_NAME_LEN);

/// Layout shared with rustspace `StatFlat`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StatFlat {
    pub name: [u8; FS_NAME_LEN],
    pub name_len: u8,
    pub is_dir: u8,
    /// `FileAttributes` bits
    pub attributes: u8,
    pub padding: [u8; 5],
    pub size: u64,
    pub created_time: u32,
    pub modified_time: u32,
}

/// Layout shared with rustspace `DirEntryFlat`
pub type DirEntryFlat = StatFlat;

const STAT_FLAT_SIZE_BYTES: usize = 40;
const_assert!(core::mem::size_of::<StatFlat>() == STAT_FLAT_SIZE_BYTES);
const_assert!(core::mem::align_of::<StatFlat>() == core::mem::align_of::<u64>());

impl StatFlat {
    pub fn from_node(node: &FileNode) -> Self {
        let name_bytes = node.name.as_bytes();
        debug_assert!(name_bytes.len() <= FS_NAME_LEN);

        let name_len = name_bytes.len().min(FS_NAME_LEN);
        let mut name = [0u8; FS_NAME_LEN];
        name[..name_len].copy_from_slice(&name_bytes[..name_len]);

        Self {
            name,
            name_len: name_len as u8,
            is_dir: (node.file_type == FileType::Directory) as u8,
            attributes: node.attributes.bits(),
            padding: [0; 5],
            size: node.size as u64,
            created_time: node.created_time,
            modified_time: node.modified_time,
        }
    }
}

// The fd is read under PROCESS_MANAGER and the lock is released before VFS is taken,
// the two are never held together
fn current_pid() -> PID {
    scheduler::get_current_process_for_core(get_current_core_id())
}

fn lookup_fd(fd: u64) -> Result<FileDescriptor, SyscallError> {
    let process_manager = PROCESS_MANAGER.lock();
    let process = process_manager
        .get_process(current_pid())
        .map_err(|_| SyscallError::ProcessNotFound)?;

    process.get_fd(fd as usize)
}

/// OpenFile(path_ptr, path_len, flags) -> fd
pub fn open_file(frame: &SyscallFrame) -> Result<u64, SyscallError> {
    let path = copy_user_path(frame.arg1 as usize, frame.arg2 as usize)?;
    let flags = frame.arg3;
    if flags == 0 || flags & !(FD_FLAGS_VALID_MASK as u64) != 0 {
        return Err(SyscallError::InvalidArgument);
    }

    let node = get_vfs().open_file(path.as_str())?;
    if flags & FD_FLAG_WRITE as u64 != 0 && !node.attributes.contains(FileAttributes::WRITE) {
        return Err(SyscallError::PermissionDenied);
    }

    let mut process_manager = PROCESS_MANAGER.lock();
    let process = process_manager
        .get_process_mut(current_pid())
        .map_err(|_| SyscallError::ProcessNotFound)?;

    process
        .open_fd(node.node_id, flags as u8)
        .map(|fd| fd as u64)
}

/// CloseFile(fd)
pub fn close_file(frame: &SyscallFrame) -> Result<u64, SyscallError> {
    let mut process_manager = PROCESS_MANAGER.lock();
    let process = process_manager
        .get_process_mut(current_pid())
        .map_err(|_| SyscallError::ProcessNotFound)?;

    process.close_fd(frame.arg1 as usize)?;

    Ok(0)
}

/// ReadFile(fd, offset, buffer_ptr, buffer_len) -> bytes read
pub fn read_file(frame: &SyscallFrame) -> Result<u64, SyscallError> {
    let descriptor = lookup_fd(frame.arg1)?;
    if descriptor.flags & FD_FLAG_READ == 0 {
        return Err(SyscallError::PermissionDenied);
    }

    let offset = frame.arg2 as usize;
    let buffer_ptr = frame.arg3 as usize;
    let buffer_len = frame.arg4 as usize;
    if !validate_user_ptr(buffer_ptr, buffer_len, UserAccess::Write) {
        return Err(SyscallError::InvalidPtr);
    }

    // SAFETY: the range was validated as mapped, writable user memory
    let buffer = unsafe { core::slice::from_raw_parts_mut(buffer_ptr as *mut u8, buffer_len) };
    let read = get_vfs().read_node(descriptor.node_id, offset, buffer)?;

    Ok(read as u64)
}

/// WriteFile(fd, offset, buffer_ptr, buffer_len) -> bytes written
pub fn write_file(frame: &SyscallFrame) -> Result<u64, SyscallError> {
    let descriptor = lookup_fd(frame.arg1)?;
    if descriptor.flags & FD_FLAG_WRITE == 0 {
        return Err(SyscallError::PermissionDenied);
    }

    let offset = frame.arg2 as usize;
    let buffer_ptr = frame.arg3 as usize;
    let buffer_len = frame.arg4 as usize;
    if !validate_user_ptr(buffer_ptr, buffer_len, UserAccess::Read) {
        return Err(SyscallError::InvalidPtr);
    }

    // SAFETY: the range was validated as mapped user memory
    let data = unsafe { core::slice::from_raw_parts(buffer_ptr as *const u8, buffer_len) };
    let written = get_vfs().write_node(descriptor.node_id, offset, data)?;

    Ok(written as u64)
}

/// CreateFile(path_ptr, path_len)
pub fn create_file(frame: &SyscallFrame) -> Result<u64, SyscallError> {
    let path = copy_user_path(frame.arg1 as usize, frame.arg2 as usize)?;
    get_vfs().create_file(path.as_str())?;

    Ok(0)
}

/// Delete(path_ptr, path_len)
pub fn delete(frame: &SyscallFrame) -> Result<u64, SyscallError> {
    let path = copy_user_path(frame.arg1 as usize, frame.arg2 as usize)?;
    get_vfs().delete(path.as_str())?;

    Ok(0)
}

/// StatFile(path_ptr, path_len, out_stat_ptr)
pub fn stat_file(frame: &SyscallFrame) -> Result<u64, SyscallError> {
    let path = copy_user_path(frame.arg1 as usize, frame.arg2 as usize)?;
    let out_ptr = frame.arg3 as usize;
    if !validate_user_ptr(out_ptr, STAT_FLAT_SIZE_BYTES, UserAccess::Write) {
        return Err(SyscallError::InvalidPtr);
    }

    let node = get_vfs().resolve_path(path.as_str())?;
    let stat = StatFlat::from_node(&node);

    // SAFETY: the range was validated as mapped, writable user memory, alignment is not assumed
    unsafe { (out_ptr as *mut StatFlat).write_unaligned(stat) };

    Ok(0)
}

/// ListDir(path_ptr, path_len, out_entries_ptr, out_entries_bytes, start_index) -> entries written
/// Writes entries start_index.. up to the buffer capacity, a short count means the end
pub fn list_dir(frame: &SyscallFrame) -> Result<u64, SyscallError> {
    let path = copy_user_path(frame.arg1 as usize, frame.arg2 as usize)?;
    let out_ptr = frame.arg3 as usize;
    let capacity = frame.arg4 as usize / STAT_FLAT_SIZE_BYTES;
    let start_index = frame.arg5 as usize;
    if !validate_user_ptr(out_ptr, capacity * STAT_FLAT_SIZE_BYTES, UserAccess::Write) {
        return Err(SyscallError::InvalidPtr);
    }

    let entries = get_vfs().list_directory(path.as_str())?;
    let page = entries.iter().skip(start_index).take(capacity);

    let mut written = 0;
    for node in page {
        // SAFETY: slot written is below capacity, inside the validated range
        unsafe {
            (out_ptr as *mut DirEntryFlat)
                .byte_add(written * STAT_FLAT_SIZE_BYTES)
                .write_unaligned(StatFlat::from_node(node));
        }
        written += 1;
    }

    Ok(written as u64)
}

/// CreateDir(path_ptr, path_len)
pub fn create_dir(frame: &SyscallFrame) -> Result<u64, SyscallError> {
    let path = copy_user_path(frame.arg1 as usize, frame.arg2 as usize)?;
    get_vfs().create_directory(path.as_str())?;

    Ok(0)
}
