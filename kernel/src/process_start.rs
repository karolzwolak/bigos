use alloc::format;
use alloc::string::String;

use crate::process::elf_loader::{BIGPROC_ELF, SHELL_ELF, load_elf_from_path};
use crate::process::process_manager::{ARCHE_PID, BIGPROC_NAME, BIGPROC_PID};
use crate::process::shared_state::init_shared_state;
use crate::process::{ElfLoadInfo, process_manager::PROCESS_MANAGER};
use crate::{RUN_SHELL, serial_println};

const SHELL_PATH: &str = "/BIN/shell";
const SHELL_NAME: &str = "shell";
const SHELL_PRIORITY: u8 = 7;
const INIT_PARENT_PID: usize = BIGPROC_PID;
// bigproc only blocks, it never competes for cpu time
const BIGPROC_PRIORITY: u8 = 0;

fn spawn_from_elf_info(
    elf_info: &ElfLoadInfo,
    name: &str,
    parent_pid: usize,
    priority: u8,
) -> Result<usize, String> {
    let mut pm = PROCESS_MANAGER.lock();
    let pid = pm
        .create_process_from_elf(parent_pid, elf_info, name, priority)
        .map_err(|e| format!("Failed to create process: {:?}", e))?;

    serial_println!(
        "Created userspace process '{}' (PID {}) with priority {}",
        name,
        pid,
        priority
    );
    Ok(pid)
}

pub fn create_userspace_process(
    elf_data: &[u8],
    name: &str,
    parent_pid: usize,
    priority: u8,
) -> Result<usize, String> {
    let elf_info = ElfLoadInfo::from_elf_data(elf_data)
        .map_err(|e| format!("Failed to parse ELF: {:?}", e))?;

    spawn_from_elf_info(&elf_info, name, parent_pid, priority)
}

/// The process name is the last component of the path, the ELF is read under the VFS lock and
/// the lock is released before PROCESS_MANAGER is taken
pub fn create_userspace_process_from_path(
    path: &str,
    parent_pid: usize,
    priority: u8,
) -> Result<usize, String> {
    let elf_info =
        load_elf_from_path(path).map_err(|e| format!("Failed to load {}: {:?}", path, e))?;
    let name = path.rsplit('/').next().unwrap_or(path);

    spawn_from_elf_info(&elf_info, name, parent_pid, priority)
}

/// Sets up the user-visible process state and starts bigproc as PID 1
/// Call once, before any other userspace process is created
pub fn init_userspace() {
    init_shared_state();

    let pid = create_userspace_process(BIGPROC_ELF, BIGPROC_NAME, ARCHE_PID, BIGPROC_PRIORITY)
        .unwrap_or_else(|error| panic!("bigproc failed to start: {}", error));
    assert!(
        pid == BIGPROC_PID,
        "bigproc got PID {} instead of {}",
        pid,
        BIGPROC_PID
    );
}

pub fn create_userspace_processes() {
    serial_println!("Creating initial userspace processes");

    if RUN_SHELL {
        match create_userspace_process_from_path(SHELL_PATH, INIT_PARENT_PID, SHELL_PRIORITY) {
            Ok(pid) => serial_println!("Created shell from {} (PID {})", SHELL_PATH, pid),
            Err(error) => {
                serial_println!("{}, falling back to the embedded shell", error);
                match create_userspace_process(
                    SHELL_ELF,
                    SHELL_NAME,
                    INIT_PARENT_PID,
                    SHELL_PRIORITY,
                ) {
                    Ok(pid) => serial_println!("Created embedded shell (PID {})", pid),
                    Err(error) => serial_println!("Embedded shell failed: {}", error),
                }
            }
        }
    }

    serial_println!("Process creation complete");
    serial_println!("All processes are now in the scheduler queues");
}
