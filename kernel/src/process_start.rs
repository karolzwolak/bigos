use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::process::elf_loader::{BIGPROC_ELF, SHELL_ELF, load_elf_from_path};
use crate::process::process_manager::{BIGPROC_NAME, BIGPROC_PID, NO_PARENT_PID};
use crate::process::shared_state::init_shared_state;
use crate::process::{ElfLoadInfo, ProcessState, process_manager::PROCESS_MANAGER};
use crate::{RUN_SHELL, serial_println};

const SHELL_PATH: &str = "/BIN/shell";
const SHELL_NAME: &str = "shell";
const SHELL_PRIORITY: u8 = 7;
const INIT_PARENT_PID: usize = BIGPROC_PID;
// bigproc only blocks, it never competes for cpu time
const BIGPROC_PRIORITY: u8 = 0;
const NO_SHELL_PID: usize = usize::MAX;

static SHELL_PID: AtomicUsize = AtomicUsize::new(NO_SHELL_PID);

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

    let pid = create_userspace_process(BIGPROC_ELF, BIGPROC_NAME, NO_PARENT_PID, BIGPROC_PRIORITY)
        .unwrap_or_else(|error| panic!("bigproc failed to start: {}", error));
    assert!(
        pid == BIGPROC_PID,
        "bigproc got PID {} instead of {}",
        pid,
        BIGPROC_PID
    );
}

fn start_shell() {
    let started =
        match create_userspace_process_from_path(SHELL_PATH, INIT_PARENT_PID, SHELL_PRIORITY) {
            Ok(pid) => {
                serial_println!("Created shell from {} (PID {})", SHELL_PATH, pid);
                Ok(pid)
            }
            Err(error) => {
                serial_println!("{}, falling back to the embedded shell", error);
                create_userspace_process(SHELL_ELF, SHELL_NAME, INIT_PARENT_PID, SHELL_PRIORITY)
            }
        };

    match started {
        Ok(pid) => SHELL_PID.store(pid, Ordering::Release),
        Err(error) => serial_println!("Shell failed to start: {}", error),
    }
}

pub fn is_shell_running() -> bool {
    let pid = SHELL_PID.load(Ordering::Acquire);
    if pid == NO_SHELL_PID {
        return false;
    }

    PROCESS_MANAGER
        .lock()
        .get_process(pid)
        .is_ok_and(|process| {
            process.state != ProcessState::Terminated && process.name == SHELL_NAME
        })
}

pub fn restart_shell_if_gone() -> bool {
    if is_shell_running() {
        return false;
    }

    start_shell();
    is_shell_running()
}

pub fn create_userspace_processes() {
    serial_println!("Creating initial userspace processes");

    if RUN_SHELL {
        start_shell();
    }

    serial_println!("Process creation complete");
    serial_println!("All processes are now in the scheduler queues");
}
