use crate::gdt::{self, USER_CODE_SELECTOR_VALUE, USER_DATA_SELECTOR_VALUE};
use crate::process::process::{INVALID_PID, PID, RFLAGS_DEFAULT};
use crate::process::scheduler;
use crate::serial_println_core;
use crate::trap::{TRAP_FRAME_SIZE, TrapFrame, pop_all_gprs};
use crate::util::cpuinfo::get_current_core_id;
use core::arch::{asm, naked_asm};
use core::cell::UnsafeCell;
use core::sync::atomic::{
    AtomicBool, AtomicI32, AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

pub const MAX_TASKS: usize = 64;
pub const INVALID_SLOT: u16 = u16::MAX;

const FPU_STATE_SIZE_BYTES: usize = 512;
const FXSAVE_FCW_OFFSET: usize = 0;
const FXSAVE_MXCSR_OFFSET: usize = 24;

// reset values, every FP exception masked
const FCW_DEFAULT: u16 = 0x37F;
const MXCSR_DEFAULT: u32 = 0x1F80;

const CALLEE_SAVED_REGISTER_COUNT: usize = 6;
const CALLEE_SAVED_SIZE: u64 = (CALLEE_SAVED_REGISTER_COUNT * core::mem::size_of::<u64>()) as u64;
const RETURN_ADDRESS_SIZE: u64 = core::mem::size_of::<u64>() as u64;

/// FXSAVE image: x87, xmm0-15 and MXCSR
/// NOTE: Holds no ymm state because CR4.OSXSAVE is off, so AVX instructions #UD
#[repr(C, align(16))]
pub struct FpuState {
    pub fxsave_area: [u8; FPU_STATE_SIZE_BYTES],
}

const _: () = assert!(core::mem::size_of::<FpuState>() == FPU_STATE_SIZE_BYTES);

impl FpuState {
    pub const fn new() -> Self {
        let mut fxsave_area = [0u8; FPU_STATE_SIZE_BYTES];
        let fcw_bytes = FCW_DEFAULT.to_le_bytes();
        let mxcsr_bytes = MXCSR_DEFAULT.to_le_bytes();

        fxsave_area[FXSAVE_FCW_OFFSET] = fcw_bytes[0];
        fxsave_area[FXSAVE_FCW_OFFSET + 1] = fcw_bytes[1];

        fxsave_area[FXSAVE_MXCSR_OFFSET] = mxcsr_bytes[0];
        fxsave_area[FXSAVE_MXCSR_OFFSET + 1] = mxcsr_bytes[1];
        fxsave_area[FXSAVE_MXCSR_OFFSET + 2] = mxcsr_bytes[2];
        fxsave_area[FXSAVE_MXCSR_OFFSET + 3] = mxcsr_bytes[3];

        Self { fxsave_area }
    }

    pub unsafe fn save_from_cpu(&mut self) {
        unsafe {
            asm!(
                "fxsave64 [{}]",
                in(reg) self.fxsave_area.as_mut_ptr(),
                options(nostack),
            );
        }
    }

    pub unsafe fn restore_to_cpu(&self) {
        unsafe {
            asm!(
                "fxrstor64 [{}]",
                in(reg) self.fxsave_area.as_ptr(),
                options(nostack, readonly),
            );
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Free = 0,
    /// Slot reserved, stack and bookkeeping are being filled in
    Creating,
    Ready,
    Running,
    Blocked,
    /// The task finished tearing itself down, the scheduler loop reaps the slot
    Dead,
}

impl TaskState {
    pub fn from_raw(raw: u8) -> Self {
        debug_assert!(raw <= TaskState::Dead as u8);

        // SAFETY: raw is only ever produced from a TaskState
        unsafe { core::mem::transmute::<u8, TaskState>(raw) }
    }
}

/// identifies a task for slot reuse, valid while the slot still holds the same pid
/// PIDs are never reused
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskRef {
    pub slot: u16,
    pub pid: PID,
}

impl TaskRef {
    pub fn is_alive(&self) -> bool {
        slot(self.slot as usize).pid.load(Ordering::Acquire) == self.pid
    }
}

pub struct TaskSlot {
    pub pid: AtomicUsize,
    pub state: AtomicU8,
    pub priority: AtomicU8,
    pub home_core: AtomicU8,
    pub on_cpu: AtomicBool,
    pub kill_pending: AtomicBool,
    pub exit_code_request: AtomicI32,
    pub cpu_ticks: AtomicU64,
    pub core_migration_count: AtomicU32,
    pub cr3: AtomicU64,
    pub kernel_stack_top: AtomicU64,
    /// Next slot in the run queue queue the task sits in
    pub next: AtomicU16,
    /// Written by switch_context while the task is switched out
    pub saved_rsp: UnsafeCell<u64>,
    pub fpu_state: UnsafeCell<FpuState>,
}

// SAFETY: saved_rsp and fpu_state are only touched by the core that owns the task
// at that moment, handoffs between cores go through the on_cpu and state atomics
unsafe impl Sync for TaskSlot {}

impl TaskSlot {
    const fn new() -> Self {
        Self {
            pid: AtomicUsize::new(INVALID_PID),
            state: AtomicU8::new(TaskState::Free as u8),
            priority: AtomicU8::new(0),
            home_core: AtomicU8::new(0),
            on_cpu: AtomicBool::new(false),
            kill_pending: AtomicBool::new(false),
            exit_code_request: AtomicI32::new(0),
            cpu_ticks: AtomicU64::new(0),
            core_migration_count: AtomicU32::new(0),
            cr3: AtomicU64::new(0),
            kernel_stack_top: AtomicU64::new(0),
            next: AtomicU16::new(INVALID_SLOT),
            saved_rsp: UnsafeCell::new(0),
            fpu_state: UnsafeCell::new(FpuState::new()),
        }
    }

    pub fn state(&self) -> TaskState {
        TaskState::from_raw(self.state.load(Ordering::Acquire))
    }

    pub fn set_state(&self, state: TaskState) {
        self.state.store(state as u8, Ordering::Release);
    }

    /// Moves from one state to another, false when the task was not in `from`
    pub fn transition(&self, from: TaskState, to: TaskState) -> bool {
        self.state
            .compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

pub static TASKS: [TaskSlot; MAX_TASKS] = [const { TaskSlot::new() }; MAX_TASKS];

pub fn slot(slot_index: usize) -> &'static TaskSlot {
    debug_assert!(slot_index < MAX_TASKS);

    &TASKS[slot_index]
}

pub fn find_slot_by_pid(pid: PID) -> Option<usize> {
    TASKS
        .iter()
        .position(|task| task.pid.load(Ordering::Acquire) == pid)
}

/// Reserves a free slot, None when all MAX_TASKS are in use
/// The slot stays Creating until the caller makes it Ready
pub fn allocate_task_slot(pid: PID, priority: u8, home_core: u8, cr3: u64) -> Option<usize> {
    for (slot_index, task) in TASKS.iter().enumerate() {
        if !task.transition(TaskState::Free, TaskState::Creating) {
            continue;
        }

        task.priority.store(priority, Ordering::Relaxed);
        task.home_core.store(home_core, Ordering::Relaxed);
        task.on_cpu.store(false, Ordering::Relaxed);
        task.kill_pending.store(false, Ordering::Relaxed);
        task.exit_code_request.store(0, Ordering::Relaxed);
        task.cpu_ticks.store(0, Ordering::Relaxed);
        task.core_migration_count.store(0, Ordering::Relaxed);
        task.cr3.store(cr3, Ordering::Relaxed);
        task.next.store(INVALID_SLOT, Ordering::Relaxed);
        task.kernel_stack_top
            .store(gdt::task_stack_top(slot_index), Ordering::Relaxed);

        // SAFETY: the slot is Creating, nobody else touches the fpu image
        unsafe { *task.fpu_state.get() = FpuState::new() };

        task.pid.store(pid, Ordering::Release);

        return Some(slot_index);
    }

    None
}

pub fn core_current_task_ref(core_id: u8) -> TaskRef {
    let slot_index = scheduler::get_current_slot_for_core(core_id);
    debug_assert!(slot_index != INVALID_SLOT as usize);

    TaskRef {
        slot: slot_index as u16,
        pid: scheduler::get_current_process_for_core(core_id),
    }
}

pub fn core_current_task_kill_pending(core_id: u8) -> bool {
    slot(core_current_task_ref(core_id).slot as usize)
        .kill_pending
        .load(Ordering::Acquire)
}

pub fn release_task_slot(slot_index: usize) {
    let task = slot(slot_index);

    task.pid.store(INVALID_PID, Ordering::Release);
    task.set_state(TaskState::Free);
}

/// Builds the stack of a task that has never run
/// The first switch_context into it returns to task_first_entry, which leaves
/// for ring 3 through the trap frame above it
pub fn init_task_stack(slot_index: usize, user_rip: u64, user_rsp: u64) {
    let task = slot(slot_index);
    let stack_top = task.kernel_stack_top.load(Ordering::Relaxed);
    debug_assert!(stack_top % 16 == 0);

    let trap_frame_addr = stack_top - TRAP_FRAME_SIZE as u64;
    let return_address_slot = trap_frame_addr - RETURN_ADDRESS_SIZE;
    let saved_rsp = return_address_slot - CALLEE_SAVED_SIZE;

    let mut frame = TrapFrame::zeroed();
    frame.rip = user_rip;
    frame.cs = USER_CODE_SELECTOR_VALUE as u64;
    frame.rflags = RFLAGS_DEFAULT;
    frame.rsp = user_rsp;
    frame.ss = USER_DATA_SELECTOR_VALUE as u64;

    // SAFETY: the addresses lie inside the stack of this slot, which nobody runs on yet
    unsafe {
        core::ptr::write(trap_frame_addr as *mut TrapFrame, frame);
        core::ptr::write(
            return_address_slot as *mut u64,
            task_first_entry as usize as u64,
        );
        core::ptr::write_bytes(saved_rsp as *mut u64, 0, CALLEE_SAVED_REGISTER_COUNT);
        *task.saved_rsp.get() = saved_rsp;
    }
}

/// Reached by the first switch_context into a task, with rsp at its trap frame
#[unsafe(naked)]
unsafe extern "C" fn task_first_entry() -> ! {
    naked_asm!(
        "mov rdi, rsp",
        "call {check}",

        "mov ax, {user_data_selector}",
        "mov ds, ax",
        "mov es, ax",
        "mov fs, ax",
        "mov gs, ax",

        pop_all_gprs!(),
        "iretq",

        check = sym task_first_entry_check,
        user_data_selector = const USER_DATA_SELECTOR_VALUE,
    )
}

extern "C" fn task_first_entry_check(frame: *const TrapFrame) {
    debug_assert!(frame as usize % 16 == 0);
    debug_assert!(unsafe { (*frame).cs } == USER_CODE_SELECTOR_VALUE as u64);

    // killed before it ever ran, so it never enters ring 3
    exit_if_kill_pending();
}

/// Called on the way to ring 3, where the task holds no kernel resources
pub fn exit_if_kill_pending() {
    let core_id = get_current_core_id();
    let slot_index = scheduler::get_current_slot_for_core(core_id);
    debug_assert!(slot_index != INVALID_SLOT as usize);

    let task = slot(slot_index);
    if task.kill_pending.load(Ordering::Acquire) {
        exit_current(task.exit_code_request.load(Ordering::Acquire));
    }
}

/// Asks a task to exit, the task tears itself down at its next return to ring 3
/// Returns false when no such task exists
pub fn request_kill(pid: PID, exit_code: i32) -> bool {
    let Some(slot_index) = find_slot_by_pid(pid) else {
        return false;
    };

    let task = slot(slot_index);
    task.exit_code_request.store(exit_code, Ordering::Release);
    task.kill_pending.store(true, Ordering::Release);

    scheduler::wake_task(TaskRef {
        slot: slot_index as u16,
        pid,
    });

    // a task running in ring 3 only notices when something takes it into the kernel
    if task.state() == TaskState::Running {
        let running_core = task.home_core.load(Ordering::Relaxed);
        if running_core == get_current_core_id() {
            scheduler::mark_core_for_reschedule(running_core);
        } else {
            crate::interrupts::send_reschedule_ipi(running_core);
        }
    }

    true
}

/// Tears the running task down on its own kernel stack, never returns
/// The scheduler loop frees the slot once it has switched away from the dead task
pub fn exit_current(exit_code: i32) -> ! {
    let core_id = get_current_core_id();
    let slot_index = scheduler::get_current_slot_for_core(core_id);
    let pid = scheduler::get_current_process_for_core(core_id);
    debug_assert!(slot_index != INVALID_SLOT as usize);

    // user page tables are freed below, so leave them first
    unsafe {
        let kernel_phys = crate::memory::get_user_mem_mgr().kernel_page_table_phys;
        let kernel_frame = x86_64::structures::paging::PhysFrame::<
            x86_64::structures::paging::Size4KiB,
        >::containing_address(kernel_phys);
        x86_64::registers::control::Cr3::write(
            kernel_frame,
            x86_64::registers::control::Cr3Flags::empty(),
        );
    }

    // window cleanup takes the process manager lock itself, so it runs first
    crate::graphics::compositor::destroy_windows_of_process(pid);
    {
        let mut process_manager = crate::process::process_manager::PROCESS_MANAGER.lock();
        let result = process_manager.terminate_process(pid, exit_code, false);
        debug_assert!(result.is_ok());
    }

    x86_64::instructions::interrupts::disable();
    let task = slot(slot_index);
    task.set_state(TaskState::Dead);
    scheduler::core_task_leave_run_queue(task.home_core.load(Ordering::Relaxed));
    scheduler::core_run_scheduling(get_current_core_id());

    unreachable!("dead task {} was scheduled again", pid);
}
