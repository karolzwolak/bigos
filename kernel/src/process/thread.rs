use crate::gdt::{self, USER_CODE_SELECTOR_VALUE, USER_DATA_SELECTOR_VALUE};
use crate::process::process_manager::BIGPROC_PID;
use crate::process::scheduler;
use crate::process::{ChildPolicy, INVALID_PID, PID, RFLAGS_DEFAULT};
use crate::trap::{TRAP_FRAME_SIZE, TrapFrame, pop_all_gprs};
use crate::util::cpuinfo::get_current_core_id;
use core::arch::{asm, naked_asm};
use core::cell::UnsafeCell;
use core::sync::atomic::{
    AtomicBool, AtomicI32, AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

pub const MAX_THREADS: usize = 64;
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

    /// # Safety
    ///
    /// SSE must be enabled on this core.
    pub unsafe fn save_from_cpu(&mut self) {
        unsafe {
            asm!(
                "fxsave64 [{}]",
                in(reg) self.fxsave_area.as_mut_ptr(),
                options(nostack),
            );
        }
    }

    /// # Safety
    ///
    /// SSE must be enabled on this core, and `self` must hold a state saved by
    /// `save_from_cpu` or a valid initial FXSAVE image.
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

impl Default for FpuState {
    fn default() -> Self {
        Self::new()
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Free = 0,
    /// Slot reserved, stack and bookkeeping are being filled in
    Creating,
    Ready,
    Running,
    Blocked,
    /// The thread finished tearing itself down, the scheduler loop removes the slot
    Dead,
}

impl ThreadState {
    pub fn from_raw(raw: u8) -> Self {
        debug_assert!(raw <= ThreadState::Dead as u8);

        // SAFETY: raw is only ever produced from a ThreadState
        unsafe { core::mem::transmute::<u8, ThreadState>(raw) }
    }
}

/// identifies a thread for slot reuse, valid while the slot still holds the same pid
/// PIDs are never reused
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadRef {
    pub slot: u16,
    pub pid: PID,
}

impl ThreadRef {
    pub fn is_alive(&self) -> bool {
        slot(self.slot as usize).pid.load(Ordering::Acquire) == self.pid
    }
}

pub struct ThreadSlot {
    pub pid: AtomicUsize,
    pub state: AtomicU8,
    pub priority: AtomicU8,
    pub assigned_core: AtomicU8,
    pub on_cpu: AtomicBool,
    pub kill_pending: AtomicBool,
    pub exit_code_request: AtomicI32,
    /// ChildPolicy of the pending kill
    pub child_policy_request: AtomicU8,
    pub cpu_ticks: AtomicU64,
    pub core_migration_count: AtomicU32,
    /// Runs that began on a core where a different thread ran last, so the registers
    /// and the fpu state really came back from the save areas
    pub handoff_count: AtomicU32,
    pub cr3: AtomicU64,
    pub kernel_stack_top: AtomicU64,
    /// Next slot in the run queue queue the thread sits in
    pub next: AtomicU16,
    /// Written by switch_context while the thread is switched out
    pub saved_rsp: UnsafeCell<u64>,
    pub fpu_state: UnsafeCell<FpuState>,
}

// SAFETY: saved_rsp and fpu_state are only touched by the core that owns the thread
// at that moment, handoffs between cores go through the on_cpu and state atomics
unsafe impl Sync for ThreadSlot {}

impl ThreadSlot {
    const fn new() -> Self {
        Self {
            pid: AtomicUsize::new(INVALID_PID),
            state: AtomicU8::new(ThreadState::Free as u8),
            priority: AtomicU8::new(0),
            assigned_core: AtomicU8::new(0),
            on_cpu: AtomicBool::new(false),
            kill_pending: AtomicBool::new(false),
            exit_code_request: AtomicI32::new(0),
            child_policy_request: AtomicU8::new(ChildPolicy::KillChildren as u8),
            cpu_ticks: AtomicU64::new(0),
            core_migration_count: AtomicU32::new(0),
            handoff_count: AtomicU32::new(0),
            cr3: AtomicU64::new(0),
            kernel_stack_top: AtomicU64::new(0),
            next: AtomicU16::new(INVALID_SLOT),
            saved_rsp: UnsafeCell::new(0),
            fpu_state: UnsafeCell::new(FpuState::new()),
        }
    }

    pub fn state(&self) -> ThreadState {
        ThreadState::from_raw(self.state.load(Ordering::Acquire))
    }

    pub fn set_state(&self, state: ThreadState) {
        self.state.store(state as u8, Ordering::Release);
    }

    /// Moves from one state to another, false when the thread was not in `from`
    pub fn transition(&self, from: ThreadState, to: ThreadState) -> bool {
        self.state
            .compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

pub static THREADS: [ThreadSlot; MAX_THREADS] = [const { ThreadSlot::new() }; MAX_THREADS];

pub fn slot(slot_index: usize) -> &'static ThreadSlot {
    debug_assert!(slot_index < MAX_THREADS);

    &THREADS[slot_index]
}

pub fn find_slot_by_pid(pid: PID) -> Option<usize> {
    THREADS
        .iter()
        .position(|thread| thread.pid.load(Ordering::Acquire) == pid)
}

/// Reserves a free slot, None when all MAX_THREADS are in use
/// The slot stays Creating until the caller makes it Ready
pub fn allocate_thread_slot(pid: PID, priority: u8, assigned_core: u8, cr3: u64) -> Option<usize> {
    for (slot_index, thread) in THREADS.iter().enumerate() {
        if !thread.transition(ThreadState::Free, ThreadState::Creating) {
            continue;
        }

        thread.priority.store(priority, Ordering::Relaxed);
        thread.assigned_core.store(assigned_core, Ordering::Relaxed);
        thread.on_cpu.store(false, Ordering::Relaxed);
        thread.kill_pending.store(false, Ordering::Relaxed);
        thread.exit_code_request.store(0, Ordering::Relaxed);
        thread
            .child_policy_request
            .store(ChildPolicy::KillChildren as u8, Ordering::Relaxed);
        thread.cpu_ticks.store(0, Ordering::Relaxed);
        thread.core_migration_count.store(0, Ordering::Relaxed);
        thread.handoff_count.store(0, Ordering::Relaxed);
        thread.cr3.store(cr3, Ordering::Relaxed);
        thread.next.store(INVALID_SLOT, Ordering::Relaxed);
        thread
            .kernel_stack_top
            .store(gdt::thread_stack_top(slot_index), Ordering::Relaxed);

        // SAFETY: the slot is Creating, nobody else touches the fpu image
        unsafe { *thread.fpu_state.get() = FpuState::new() };

        thread.pid.store(pid, Ordering::Release);

        return Some(slot_index);
    }

    None
}

pub fn core_current_thread_ref(core_id: u8) -> ThreadRef {
    let slot_index = scheduler::get_current_slot_for_core(core_id);
    debug_assert!(slot_index != INVALID_SLOT as usize);

    ThreadRef {
        slot: slot_index as u16,
        pid: scheduler::get_current_process_for_core(core_id),
    }
}

pub fn core_current_thread_kill_pending(core_id: u8) -> bool {
    slot(core_current_thread_ref(core_id).slot as usize)
        .kill_pending
        .load(Ordering::Acquire)
}

pub fn release_thread_slot(slot_index: usize) {
    let thread = slot(slot_index);

    thread.pid.store(INVALID_PID, Ordering::Release);
    thread.set_state(ThreadState::Free);
}

/// Registers a thread finds on its first entry to ring 3
/// rdi and rsi carry argc and the ArgSlice array, so the entry function gets them as arguments
#[derive(Debug, Clone, Copy)]
pub struct UserEntry {
    pub rip: u64,
    pub rsp: u64,
    pub argc: u64,
    pub argv: u64,
}

/// Builds the stack of a thread that has never run
/// The first switch_context into it returns to thread_first_entry, which leaves
/// for ring 3 through the trap frame above it
pub fn init_thread_stack(slot_index: usize, entry: UserEntry) {
    let thread = slot(slot_index);
    let stack_top = thread.kernel_stack_top.load(Ordering::Relaxed);
    debug_assert!(stack_top % 16 == 0);

    let trap_frame_addr = stack_top - TRAP_FRAME_SIZE as u64;
    let return_address_slot = trap_frame_addr - RETURN_ADDRESS_SIZE;
    let saved_rsp = return_address_slot - CALLEE_SAVED_SIZE;

    let mut frame = TrapFrame::zeroed();
    frame.rip = entry.rip;
    frame.cs = USER_CODE_SELECTOR_VALUE as u64;
    frame.rflags = RFLAGS_DEFAULT;
    frame.rsp = entry.rsp;
    frame.rdi = entry.argc;
    frame.rsi = entry.argv;
    frame.ss = USER_DATA_SELECTOR_VALUE as u64;

    // SAFETY: the addresses lie inside the stack of this slot, which nobody runs on yet
    unsafe {
        core::ptr::write(trap_frame_addr as *mut TrapFrame, frame);
        core::ptr::write(
            return_address_slot as *mut u64,
            thread_first_entry as *const () as usize as u64,
        );
        core::ptr::write_bytes(saved_rsp as *mut u64, 0, CALLEE_SAVED_REGISTER_COUNT);
        *thread.saved_rsp.get() = saved_rsp;
    }
}

/// Reached by the first switch_context into a thread, with rsp at its trap frame
#[unsafe(naked)]
unsafe extern "C" fn thread_first_entry() -> ! {
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

        check = sym thread_first_entry_check,
        user_data_selector = const USER_DATA_SELECTOR_VALUE,
    )
}

extern "C" fn thread_first_entry_check(frame: *const TrapFrame) {
    debug_assert!(frame as usize % 16 == 0);
    debug_assert!(unsafe { (*frame).cs } == USER_CODE_SELECTOR_VALUE as u64);

    // killed before it ever ran, so it never enters ring 3
    exit_if_kill_pending();
}

/// Called on the way to ring 3, where the thread holds no kernel resources
pub fn exit_if_kill_pending() {
    let core_id = get_current_core_id();
    let slot_index = scheduler::get_current_slot_for_core(core_id);
    debug_assert!(slot_index != INVALID_SLOT as usize);

    let thread = slot(slot_index);
    if thread.kill_pending.load(Ordering::Acquire) {
        let child_policy =
            ChildPolicy::from_raw(thread.child_policy_request.load(Ordering::Acquire));
        exit_current(
            thread.exit_code_request.load(Ordering::Acquire),
            child_policy,
        );
    }
}

/// Asks a thread to exit, the thread tears itself down at its next return to ring 3
/// Returns false when no such thread exists
pub fn request_kill(pid: PID, exit_code: i32, child_policy: ChildPolicy) -> bool {
    debug_assert!(pid != BIGPROC_PID, "bigproc is never killed");

    let Some(slot_index) = find_slot_by_pid(pid) else {
        return false;
    };

    let thread = slot(slot_index);
    thread.exit_code_request.store(exit_code, Ordering::Release);
    thread
        .child_policy_request
        .store(child_policy as u8, Ordering::Release);
    thread.kill_pending.store(true, Ordering::Release);

    scheduler::wake_thread(ThreadRef {
        slot: slot_index as u16,
        pid,
    });

    // a thread running in ring 3 only notices when something takes it into the kernel
    if thread.state() == ThreadState::Running {
        let assigned_core = thread.assigned_core.load(Ordering::Relaxed);
        if assigned_core == get_current_core_id() {
            scheduler::mark_core_for_reschedule(assigned_core);
        } else {
            crate::interrupts::send_reschedule_ipi(assigned_core);
        }
    }

    true
}

/// Tears the running thread down on its own kernel stack, never returns
/// The scheduler loop frees the slot once it has switched away from the dead thread
pub fn exit_current(exit_code: i32, child_policy: ChildPolicy) -> ! {
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
        let result = process_manager.terminate_process(pid, exit_code, child_policy);
        debug_assert!(result.is_ok());
    }
    crate::process::wait::PROCESS_EXIT_WAIT_QUEUE.wake_all();

    x86_64::instructions::interrupts::disable();
    let thread = slot(slot_index);
    thread.set_state(ThreadState::Dead);
    scheduler::core_thread_leave_run_queue(thread.assigned_core.load(Ordering::Relaxed));
    scheduler::core_run_scheduling(get_current_core_id());

    unreachable!("dead thread {} was scheduled again", pid);
}
