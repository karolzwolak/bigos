use crate::data_structures::vector::Vec;
use crate::process::process::ProcessResources;
//use alloc::vec::Vec;
use crate::memory::usermem::USER_STACK_TOP;
use crate::process::core_pool::CORE_POOL;
use crate::process::elf_loader::ElfLoadError;
use crate::process::process::{INVALID_PID, MAX_PRIORITY, PID, Process, ProcessState};
use crate::process::scheduler;
use crate::process::task::{self, TaskState};
use crate::{serial_println, serial_println_core};
use alloc::string::String;
use spin::Mutex;
use x86_64::PhysAddr;
use x86_64::instructions::interrupts;

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
    NoCoresAvailable,
    SchedulerError,
    OutOfTaskSlots,
}

/// Manages processes and their associated kernel threads
/// Implements task parallelism using a one-to-one threading model
pub struct ProcessManager {
    processes: Vec<Process>,
    new_pid: usize,
}

unsafe impl Send for ProcessManager {}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Vec::with_capacity(16),
            new_pid: 1,
        }
    }

    pub fn init_arche(&mut self) -> usize {
        let arche = Process::new(
            ARCHE_PID,
            ARCHE_PID,
            MAX_PRIORITY,
            String::from("arche"),
            true,
            ProcessResources {
                memory_limit: usize::MAX,
                memory_used: 0,
                cpu_time_slice: 0,
            },
            0,
        );
        serial_println!("Initialized arche process with PID 0");
        self.processes.push(arche.unwrap());
        0
    }

    pub fn create_process_from_elf(
        &mut self,
        parent_pid: usize,
        elf_info: &crate::process::ElfLoadInfo,
        name: &str,
        priority: u8,
    ) -> Result<usize, ProcessError> {
        // Verify parent exists
        if crate::process::PROCESS_DEBUG_PRINT {
            serial_println!("create_process_from_elf");
        }

        let _parent = self
            .get_process(parent_pid)
            .map_err(|_| ProcessError::ParentNotFound)?;

        let new_pid = self.new_pid;
        self.new_pid += 1;

        if crate::process::PROCESS_DEBUG_PRINT {
            serial_println!("create_process_from_elf: assigned pid: {}", new_pid);
        }

        let total_cores = CORE_POOL.lock().total_cores() as usize;
        let core_id = {
            let mut best_core = 1u8;
            let mut best_count = usize::MAX;
            for c in 1..total_cores {
                let queued = scheduler::core_runnable_tasks_count(c as u8);
                if queued < best_count {
                    best_count = queued;
                    best_core = c as u8;
                }
            }
            best_core
        };

        if crate::process::PROCESS_DEBUG_PRINT {
            serial_println!("create_process_from_elf: assigned Core ID: {}", core_id);
        }

        // Create process structure with ELF-loaded memory
        let mut process = Process::create_with_elf(elf_info, name, new_pid, parent_pid)
            .map_err(|_| ProcessError::SchedulerError)?;

        // Set priority and state
        process.priority = priority;
        process.state = ProcessState::Ready;

        let cr3 = process.memory_layout.top_page_table_phys.as_u64();
        let Some(slot_index) = task::allocate_task_slot(new_pid, priority, core_id, cr3) else {
            let user_mgr = crate::memory::get_user_mem_mgr();
            let mut frame_allocator = crate::memory::get_frame_allocator();
            process
                .memory_layout
                .free_address_space(user_mgr, &mut frame_allocator);
            return Err(ProcessError::OutOfTaskSlots);
        };
        task::init_task_stack(slot_index, elf_info.entry_point, USER_STACK_TOP);

        // Store the process BEFORE enqueuing to the scheduler, an AP core can pick the
        // task up the instant it is queued
        self.processes.push(process);

        task::slot(slot_index).set_state(TaskState::Ready);
        scheduler::core_enqueue_ready_task(core_id, slot_index, priority);

        if crate::process::PROCESS_DEBUG_PRINT {
            serial_println!(
                "create_process_from_elf: enqueued process PID {} on core {}; priority: {}",
                new_pid,
                core_id,
                priority
            );
        }

        if crate::process::PROCESS_DEBUG_PRINT {
            serial_println!(
                "Created userspace process {} (PID {}) from ELF on core {} with priority {}",
                name,
                new_pid,
                core_id,
                priority
            );
        }

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

        let children: Vec<PID>;
        {
            let process = self.get_process_mut(pid)?;
            children = process.children.clone();
            process.state = ProcessState::Terminated;
            process.exit_code = Some(exit_code);

            if process.memory_layout.top_page_table_phys.as_u64() != 0 {
                let user_mgr = crate::memory::get_user_mem_mgr();
                let mut frame_allocator = crate::memory::get_frame_allocator();
                process
                    .memory_layout
                    .free_address_space(user_mgr, &mut frame_allocator);
                process.memory_layout.top_page_table_phys = PhysAddr::new(0);
                process.memory_layout.regions.clear();
                if crate::process::PROCESS_DEBUG_PRINT {
                    serial_println!(
                        "terminate_process: freed address space for PID {} (free frames: {})",
                        pid,
                        frame_allocator.free_frame_count()
                    );
                }
            }
        }

        // Handle children
        if cascade {
            for child_pid in children.iter().copied() {
                task::request_kill(child_pid, exit_code);
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

    pub fn cleanup_dead(&mut self) {
        self.processes
            .retain(|p| p.state != ProcessState::Terminated);
    }

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

    pub fn process_count(&self) -> usize {
        self.processes.len()
    }
}
