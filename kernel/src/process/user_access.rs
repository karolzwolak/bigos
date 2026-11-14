use crate::memory::paging::PAGE_SIZE;
use crate::memory::usermem::USER_MEM_MAX_ADDRESS;
use crate::process::syscall::SyscallError;
use x86_64::registers::control::Cr3;

pub const MAX_PATH_LEN: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserAccess {
    Read,
    Write,
}

/// Checks that the whole range lies in user space and that every page in it is mapped
/// for user access on the page tables of the running process
pub fn validate_user_ptr(ptr: usize, len: usize, access: UserAccess) -> bool {
    let Some(end) = ptr.checked_add(len) else {
        return false;
    };
    if ptr == 0 || end > USER_MEM_MAX_ADDRESS {
        return false;
    }
    if len == 0 {
        return true;
    }

    let user_memory_manager = crate::memory::get_user_mem_mgr();
    let page_table_phys = Cr3::read().0.start_address();
    let writable = access == UserAccess::Write;

    let first_page = ptr & !(PAGE_SIZE - 1);
    (first_page..end).step_by(PAGE_SIZE).all(|page| {
        user_memory_manager.is_user_page_accessible(page_table_phys, page as u64, writable)
    })
}

/// A path copied out of user memory into a fixed buffer
pub struct PathBuf {
    pub bytes: [u8; MAX_PATH_LEN],
    pub len: usize,
}

impl PathBuf {
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_PATH_LEN],
            len: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        debug_assert!(self.len <= MAX_PATH_LEN);
        debug_assert!(core::str::from_utf8(&self.bytes[..self.len]).is_ok());

        // SAFETY: copy_user_path only stores validated UTF-8
        unsafe { core::str::from_utf8_unchecked(&self.bytes[..self.len]) }
    }
}

impl Default for PathBuf {
    fn default() -> Self {
        Self::new()
    }
}

pub fn copy_user_path(ptr: usize, len: usize) -> Result<PathBuf, SyscallError> {
    if len == 0 || len > MAX_PATH_LEN {
        return Err(SyscallError::InvalidPath);
    }
    if !validate_user_ptr(ptr, len, UserAccess::Read) {
        return Err(SyscallError::InvalidPtr);
    }

    let mut path = PathBuf::new();
    // SAFETY: the range was validated as mapped user memory above
    let source = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    path.bytes[..len].copy_from_slice(source);
    path.len = len;

    // checked on the copy, user memory may change after the copy
    if core::str::from_utf8(&path.bytes[..len]).is_err() {
        return Err(SyscallError::InvalidPath);
    }

    Ok(path)
}

/// Copies `destination.len()` bytes from user memory
pub fn copy_from_user(ptr: usize, destination: &mut [u8]) -> Result<(), SyscallError> {
    if destination.is_empty() {
        return Ok(());
    }
    if !validate_user_ptr(ptr, destination.len(), UserAccess::Read) {
        return Err(SyscallError::InvalidPtr);
    }

    // SAFETY: the range was validated as mapped user memory above
    let source = unsafe { core::slice::from_raw_parts(ptr as *const u8, destination.len()) };
    destination.copy_from_slice(source);

    Ok(())
}
