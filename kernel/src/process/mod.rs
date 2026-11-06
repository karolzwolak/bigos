/// Logs of process creation and teardown, off because every serial byte costs about 87 us
pub const PROCESS_DEBUG_PRINT: bool = false;

pub mod core_pool;
pub mod elf_loader;
pub mod process;
pub mod process_manager;
pub mod process_mem;
pub mod scheduler;
pub mod shared_state;
pub mod syscall;
pub mod task;
pub mod wait;

pub use core_pool::{CORE_POOL, CorePool};
pub use elf_loader::{ElfLoadError, ElfLoadInfo};
pub use process::{PID, Process};
pub use process_manager::ProcessManager;
