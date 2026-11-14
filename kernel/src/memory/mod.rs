/// Logs for address space creation and mapping
pub const USER_MEMORY_DEBUG_PRINT: bool = false;

pub mod allocator;
pub mod paging;
pub mod usermem;

use crate::memory::paging::MemoryMapFrameAllocator;
use crate::memory::usermem::UserMemoryManager;
use spin::{Mutex, MutexGuard, Once};
use x86_64::structures::paging::OffsetPageTable;

pub static FRAME_ALLOCATOR: Once<Mutex<MemoryMapFrameAllocator>> = Once::new();
/// Lock order: KERNEL_MAPPER before FRAME_ALLOCATOR
pub static KERNEL_MAPPER: Once<Mutex<OffsetPageTable<'static>>> = Once::new();
pub static USER_MEMORY_MANAGER: Once<UserMemoryManager> = Once::new();

pub fn init_memory_globals(
    frame_allocator: MemoryMapFrameAllocator,
    user_mem_manager: UserMemoryManager,
    kernel_mapper: OffsetPageTable<'static>,
) {
    KERNEL_MAPPER.call_once(|| Mutex::new(kernel_mapper));
    FRAME_ALLOCATOR.call_once(|| Mutex::new(frame_allocator));
    USER_MEMORY_MANAGER.call_once(|| user_mem_manager);
}

pub fn get_frame_allocator() -> MutexGuard<'static, MemoryMapFrameAllocator> {
    FRAME_ALLOCATOR.get().unwrap().lock()
}

pub fn get_kernel_mapper() -> MutexGuard<'static, OffsetPageTable<'static>> {
    KERNEL_MAPPER.get().unwrap().lock()
}

pub fn get_user_mem_mgr() -> &'static UserMemoryManager {
    USER_MEMORY_MANAGER.get().unwrap()
}
