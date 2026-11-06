/// Logs for address space creation and mapping
pub const USER_MEMORY_DEBUG_PRINT: bool = false;

pub mod allocator;
pub mod memory;
pub mod usermem;

use crate::memory::memory::MemoryMapFrameAllocator;
use crate::memory::usermem::UserMemoryManager;
use spin::{Mutex, MutexGuard, Once};

pub static FRAME_ALLOCATOR: Once<Mutex<MemoryMapFrameAllocator>> = Once::new();
pub static USER_MEMORY_MANAGER: Once<UserMemoryManager> = Once::new();

pub fn init_memory_globals(
    frame_allocator: MemoryMapFrameAllocator,
    user_mem_manager: UserMemoryManager,
) {
    FRAME_ALLOCATOR.call_once(|| Mutex::new(frame_allocator));
    USER_MEMORY_MANAGER.call_once(|| user_mem_manager);
}

pub fn get_frame_allocator() -> MutexGuard<'static, MemoryMapFrameAllocator> {
    FRAME_ALLOCATOR.get().unwrap().lock()
}

pub fn get_user_mem_mgr() -> &'static UserMemoryManager {
    USER_MEMORY_MANAGER.get().unwrap()
}
