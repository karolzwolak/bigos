use crate::data_structures::vector::Vec;
use crate::ipc::channel::{self, ChannelId, NO_CHANNEL};
use crate::process::ProcessResources;
//use alloc::vec::Vec;
use crate::process::core_pool::CORE_POOL;
use crate::process::elf_loader::ElfLoadError;
use crate::process::scheduler;
use crate::process::syscall::SyscallError;
use crate::process::task::{self, TaskState};
use crate::process::{
    ChildPolicy, FileDescriptor, INVALID_PID, MAX_OPEN_FILES, MAX_PRIORITY, MAX_PROCESS_NAME_LEN,
    PID, Process, ProcessState,
};
use crate::serial_println;
use alloc::string::String;
use spin::Mutex;
use x86_64::PhysAddr;

pub const ARCHE_PID: usize = 0;
/// The first userspace process, it adopts orphans and lives as long as the kernel
pub const BIGPROC_PID: usize = 1;
pub const BIGPROC_NAME: &str = "bigproc";

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
    Channel(SyscallError),
}

/// Everything a new process starts with besides its ELF
pub struct ProcessSpawn<'a> {
    pub parent_pid: PID,
    pub name: &'a str,
    pub priority: u8,
    pub argc: u32,
    /// argc NUL-terminated strings, already validated
    pub packed_args: &'a [u8],
    pub file_descriptors: [FileDescriptor; MAX_OPEN_FILES],
    /// The process asking for the spawn, it must be the reader of stdout_channel
    pub spawning_pid: PID,
    /// NO_CHANNEL leaves the child's stdout on serial
    pub stdout_channel: ChannelId,
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

    /// Spawn without arguments and without open files
    pub fn create_process_from_elf(
        &mut self,
        parent_pid: usize,
        elf_info: &crate::process::ElfLoadInfo,
        name: &str,
        priority: u8,
    ) -> Result<usize, ProcessError> {
        let spawn = ProcessSpawn {
            parent_pid,
            name,
            priority,
            argc: 0,
            packed_args: &[],
            file_descriptors: [FileDescriptor::CLOSED; MAX_OPEN_FILES],
            spawning_pid: parent_pid,
            stdout_channel: NO_CHANNEL,
        };

        self.spawn(&spawn, elf_info)
    }

    pub fn spawn(
        &mut self,
        spawn: &ProcessSpawn,
        elf_info: &crate::process::ElfLoadInfo,
    ) -> Result<usize, ProcessError> {
        let parent_pid = spawn.parent_pid;
        let name = spawn.name;
        let priority = spawn.priority;
        debug_assert!(!name.is_empty() && name.len() <= MAX_PROCESS_NAME_LEN);
        debug_assert!(priority as usize <= scheduler::MAX_PRIORITY);

        if crate::process::PROCESS_DEBUG_PRINT {
            serial_println!("spawn");
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
        process.file_descriptors = spawn.file_descriptors;

        // mapped before the task exists, so the child's first write already lands in the ring
        if spawn.stdout_channel != NO_CHANNEL
            && let Err(error) = channel::attach_stdout_writer(
                &mut process,
                spawn.spawning_pid,
                spawn.stdout_channel,
            )
        {
            let user_mgr = crate::memory::get_user_mem_mgr();
            let mut frame_allocator = crate::memory::get_frame_allocator();
            process
                .memory_layout
                .free_address_space(user_mgr, &mut frame_allocator);
            return Err(ProcessError::Channel(error));
        }

        // after the attach, the startup info tells the child where its stdout channel is
        let user_entry =
            process.place_arguments(elf_info.entry_point, spawn.argc, spawn.packed_args);

        let cr3 = process.memory_layout.top_page_table_phys.as_u64();
        let Some(slot_index) = task::allocate_task_slot(new_pid, priority, core_id, cr3) else {
            {
                let user_mgr = crate::memory::get_user_mem_mgr();
                let mut frame_allocator = crate::memory::get_frame_allocator();
                process
                    .memory_layout
                    .free_address_space(user_mgr, &mut frame_allocator);
            }
            channel::release_process_channels(&mut process);
            return Err(ProcessError::OutOfTaskSlots);
        };
        task::init_task_stack(slot_index, user_entry);

        // Store the process and link it to its parent BEFORE enqueuing to the scheduler,
        // an AP core can pick the task up the instant it is queued
        self.processes.push(process);
        self.get_process_mut(parent_pid)?.children.push(new_pid);

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
        child_policy: ChildPolicy,
    ) -> Result<(), ProcessError> {
        if pid == INVALID_PID {
            return Err(ProcessError::DoubleDelete);
        }
        assert!(
            pid != BIGPROC_PID,
            "bigproc (PID 1) terminated, orphans have no adopter"
        );

        let parent_pid;
        let children;
        {
            let process = self.get_process_mut(pid)?;
            parent_pid = process.parent_pid;
            children = core::mem::take(&mut process.children);
            process.state = ProcessState::Terminated;
            process.exit_code = Some(exit_code);
            process.close_all_fds();

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

            // the views went with the address space, this drops the references and tells peers
            channel::release_process_channels(process);
        }

        // a parent killed in a cascade may already be gone
        if let Ok(parent) = self.get_process_mut(parent_pid) {
            parent.children.retain(|&child_pid| child_pid != pid);
        }

        // children unlink themselves when they terminate, so every pid here is alive
        match child_policy {
            ChildPolicy::KillChildren => {
                for child_pid in children.iter().copied() {
                    let found = task::request_kill(child_pid, exit_code, ChildPolicy::KillChildren);
                    debug_assert!(found, "child {} of {} has no task", child_pid, pid);
                }
            }
            ChildPolicy::OrphanChildren => {
                for child_pid in children.iter().copied() {
                    self.get_process_mut(child_pid)?.parent_pid = BIGPROC_PID;
                }

                let bigproc = self.get_process_mut(BIGPROC_PID)?;
                for child_pid in children.iter().copied() {
                    bigproc.children.push(child_pid);
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

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}
