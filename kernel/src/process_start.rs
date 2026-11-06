use alloc::format;
use alloc::string::String;

use crate::process::elf_loader::{
    FS_CACHED_TEST_ELF, FS_TEST_ELF, GAME_ELF, ODYS_ELF, RUST_FIRST_ELF, THEOPHE_ELF,
};
use crate::process::{ElfLoadInfo, process_manager::PROCESS_MANAGER};
use crate::{
    RUN_FS_CACHED_TEST, RUN_FS_TEST, RUN_ODYS, RUN_THEOPHE, USE_GAME_PROGRAM,
    USE_RUST_USER_PROGRAMS, serial_println,
};

pub fn create_userspace_process(
    elf_data: &[u8],
    name: &str,
    parent_pid: usize,
    priority: u8,
) -> Result<usize, String> {
    let elf_info = ElfLoadInfo::from_elf_data(elf_data)
        .map_err(|e| format!("Failed to parse ELF: {:?}", e))?;

    let mut pm = PROCESS_MANAGER.lock();
    let pid = pm
        .create_process_from_elf(parent_pid, &elf_info, name, priority)
        .map_err(|e| format!("Failed to create process: {:?}", e))?;

    serial_println!(
        "Created userspace process '{}' (PID {}) with priority {}",
        name,
        pid,
        priority
    );
    Ok(pid)
}

#[allow(dead_code)]
pub fn create_userspace_processes() {
    serial_println!("Creating initial userspace processes");

    if RUN_THEOPHE {
        if let Ok(pid) = create_userspace_process(&THEOPHE_ELF, "theophe", 0, 7) {
            serial_println!("Created theophe (PID {})", pid);
        }
    }

    if RUN_ODYS {
        if let Ok(pid) = create_userspace_process(&ODYS_ELF, "odys", 0, 7) {
            serial_println!("Created odys (PID {})", pid);
        }
    }

    if RUN_FS_TEST {
        if let Ok(pid) = create_userspace_process(&FS_TEST_ELF, "fs_test", 0, 5) {
            serial_println!("Created fs_test (PID {})", pid);
        }
    }
    if RUN_FS_CACHED_TEST {
        if let Ok(pid) = create_userspace_process(&FS_CACHED_TEST_ELF, "fs_cached_test", 0, 5) {
            serial_println!("Created fs_cached_test (PID {})", pid);
        }
    }

    serial_println!("Process creation complete");
    serial_println!("All processes are now in the scheduler queues");
}
