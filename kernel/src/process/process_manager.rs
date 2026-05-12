use crate::data_structures::vector::Vec;
use crate::process::elf_loader::ElfLoadError;
use crate::process::task::{INVALID_PID, Process, ProcessState};
use crate::serial_println;
use spin::Mutex;

pub const ARCHE_PID: usize = 0;

lazy_static::lazy_static! {
    pub static ref PROCESS_MANAGER: Mutex<ProcessManager> = {
        let mut pm = ProcessManager::new();
        pm.init_arche();
        Mutex::new(pm)
    };
}

#[derive(Debug)]
pub enum ProcessError {
    ProcessNotFound,
    ParentNotFound,
    DoubleDelete,
    ElfLoadError(ElfLoadError),
}

pub struct ProcessManager {
    processes: Vec<Process>,
    new_pid: usize,
}

unsafe impl Send for ProcessManager {}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Vec::with_capacity(16),
            new_pid: 1,
        }
    }

    pub fn init_arche(&mut self) -> usize {
        // let arche = Process::new(
        //     ARCHE_PID,
        //     ARCHE_PID,
        //     MAX_PRIORITY,
        //     String::from("arche"),
        //     true,
        //     ProcessResources {
        //         memory_limit: usize::MAX,
        //         memory_used: 0,
        //         cpu_time_slice: 0,
        //     },
        //     0,
        //     0,
        //     0,
        // );
        serial_println!("Initialized arche process with PID 0");
        // self.processes.push(arche);
        0
    }

    pub fn create_process(
        &mut self,
        parent_pid: usize,
        _priority: u8,
        _name_ptr: *const u8,
        _name_len: u8,
        _is_out: bool,
    ) -> Result<usize, ProcessError> {
        assert!(
            parent_pid != INVALID_PID,
            "Parent PID cannot be INVALID_PID"
        );

        let new_pid = self.new_pid;
        self.new_pid += 1;

        // let parent = self.get_process_mut(parent_pid)?;

        // //TODO:
        // //let child_resources = if is_out {
        // let resources = ProcessResources {
        //     memory_limit: parent.resources.memory_limit / 4,
        //     memory_used: 0,
        //     cpu_time_slice: parent.resources.cpu_time_slice / 2,
        // };

        // let name_str = unsafe {
        //     let slice = core::slice::from_raw_parts(name_ptr, name_len as usize);
        //     String::from_utf8_lossy(slice).into_owned()
        // };

        // let mut new_process = Process::new(
        //     new_pid, parent_pid, priority, name_str, is_out, resources,
        //     0, // TODO: will be set when loading program
        //     0, // TODO: will be set when allocating process memory
        //     0, //TODO: will be set when creating process page tables
        // );
        // new_process.state = ProcessState::Ready;

        // parent.children.push(new_pid);

        // self.processes.push(new_process);
        Ok(new_pid)
    }

    pub fn terminate_process(
        &mut self,
        pid: usize,
        exit_code: i32,
        cascade: bool,
    ) -> Result<(), ProcessError> {
        if pid == INVALID_PID {
            return Err(ProcessError::DoubleDelete);
        }

        let children = {
            let process = self.get_process_mut(pid)?;
            process.state = ProcessState::Terminated;
            process.exit_code = Some(exit_code);
            process.children.clone()
        };

        if let Ok(parent) = self.get_process_mut(pid) {
            for child_pid in parent.children.iter_mut() {
                if *child_pid == pid {
                    *child_pid = INVALID_PID;
                }
            }
        }

        if cascade {
            for child_pid in children.iter().copied() {
                let _ = self.terminate_process(child_pid, exit_code, true);
            }
        } else {
            for child_pid in children.iter().copied() {
                if let Ok(child) = self.get_process_mut(child_pid) {
                    child.parent_pid = ARCHE_PID;
                }

                if let Ok(arche) = self.get_process_mut(ARCHE_PID) {
                    arche.children.push(child_pid);
                }
            }
        }

        Ok(())
    }

    pub fn cleanup_dead(&mut self) {}

    pub fn get_process(&self, pid: usize) -> Result<&Process, ProcessError> {
        assert!(pid != INVALID_PID, "get_process: PID cannot be INVALID_PID");
        self.processes
            .iter()
            .find(|p| p.pid == pid)
            .ok_or(ProcessError::ProcessNotFound)
    }

    pub fn get_process_mut(&mut self, pid: usize) -> Result<&mut Process, ProcessError> {
        assert!(
            pid != INVALID_PID,
            "get_process_mut: PID cannot be INVALID_PID"
        );
        self.processes
            .iter_mut()
            .find(|p| p.pid == pid)
            .ok_or(ProcessError::ProcessNotFound)
    }

    pub fn get_process_list(&self) -> &Vec<Process> {
        &self.processes
    }
}
