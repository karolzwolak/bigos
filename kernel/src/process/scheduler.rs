use crate::process::PID;
use crate::process::process::INVALID_PID;
use crate::process::task::{self, INVALID_SLOT, TaskRef, TaskState};
use crate::util::cpuinfo::get_current_core_id;
use crate::util::irq_mutex::IrqMutex;
use crate::{MAX_CORES, serial_println_core};
use core::arch::naked_asm;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
use x86_64::PhysAddr;
use x86_64::instructions::interrupts;
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::structures::paging::PhysFrame;

const SCHED_DEBUG_PRINT: bool = false;

/// An idle core takes a waiting task from the run queue of a busy core
const WORK_STEALING_ENABLED: bool = true;

// note non_empty_levels is u8 right now, increase size if we ever increase max prio, but why
pub const MAX_PRIORITY: usize = 7;
const PRIORITY_LEVEL_COUNT: usize = MAX_PRIORITY + 1;
const _: () = assert!(PRIORITY_LEVEL_COUNT <= u8::BITS as usize);

/// Timer ticks a task of priority 0 runs before it has to give way
const BASE_SLICE_TICKS: u32 = 1;

static CURRENT_PROCESS_ON_CORE: [AtomicU64; MAX_CORES as usize] =
    [const { AtomicU64::new(INVALID_PID as u64) }; MAX_CORES as usize];

static CURRENT_SLOT_ON_CORE: [AtomicU16; MAX_CORES as usize] =
    [const { AtomicU16::new(INVALID_SLOT) }; MAX_CORES as usize];

/// Set when the task running on a core should call core_run_scheduling() before it returns to ring 3
static CORE_SHOULD_BE_RESCHEDULED: [AtomicBool; MAX_CORES as usize] =
    [const { AtomicBool::new(false) }; MAX_CORES as usize];

static SLICE_REMAINING_TICKS: [AtomicU32; MAX_CORES as usize] =
    [const { AtomicU32::new(0) }; MAX_CORES as usize];

/// Tasks that are queued or running on each core, readable from interrupt context without a queue lock
static RUNNABLE_TASKS_COUNT: [AtomicU32; MAX_CORES as usize] =
    [const { AtomicU32::new(0) }; MAX_CORES as usize];

/// Stack pointer of each core's scheduler loop while a task runs
/// switch_context stores it, core_run_scheduling() loads it to get back
static mut SCHEDULER_RSP: [u64; MAX_CORES as usize] = [0u64; MAX_CORES as usize];

#[repr(align(64))]
pub struct CacheAligned<T>(pub T);

#[derive(Clone, Copy)]
struct SlotQueue {
    head: u16,
    tail: u16,
}

impl SlotQueue {
    const fn new() -> Self {
        Self {
            head: INVALID_SLOT,
            tail: INVALID_SLOT,
        }
    }

    fn push_back(&mut self, slot_index: usize) {
        let task = task::slot(slot_index);
        task.next.store(INVALID_SLOT, Ordering::Relaxed);

        if self.tail == INVALID_SLOT {
            self.head = slot_index as u16;
        } else {
            task::slot(self.tail as usize)
                .next
                .store(slot_index as u16, Ordering::Relaxed);
        }
        self.tail = slot_index as u16;
    }

    fn pop_front(&mut self) -> usize {
        debug_assert!(self.head != INVALID_SLOT);

        let slot_index = self.head as usize;
        let next = task::slot(slot_index).next.load(Ordering::Relaxed);
        self.head = next;
        if next == INVALID_SLOT {
            self.tail = INVALID_SLOT;
        }

        slot_index
    }

    /// Unlinks the first task the predicate accepts, INVALID_SLOT when none does
    fn remove_first_where(&mut self, accepts: impl Fn(usize) -> bool) -> usize {
        let mut previous = INVALID_SLOT;
        let mut cursor = self.head;

        while cursor != INVALID_SLOT {
            let task = task::slot(cursor as usize);
            let next = task.next.load(Ordering::Relaxed);

            if accepts(cursor as usize) {
                if previous == INVALID_SLOT {
                    self.head = next;
                } else {
                    task::slot(previous as usize)
                        .next
                        .store(next, Ordering::Relaxed);
                }
                if self.tail == cursor {
                    self.tail = previous;
                }

                return cursor as usize;
            }

            previous = cursor;
            cursor = next;
        }

        INVALID_SLOT as usize
    }

    fn contains(&self, slot_index: usize) -> bool {
        let mut cursor = self.head;
        while cursor != INVALID_SLOT {
            if cursor as usize == slot_index {
                return true;
            }
            cursor = task::slot(cursor as usize).next.load(Ordering::Relaxed);
        }

        false
    }
}

struct TaskSlotQueueTable {
    queues: [SlotQueue; PRIORITY_LEVEL_COUNT],
    non_empty_levels: u8, // bitmap
}

impl TaskSlotQueueTable {
    const fn new() -> Self {
        Self {
            queues: [SlotQueue::new(); PRIORITY_LEVEL_COUNT],
            non_empty_levels: 0,
        }
    }

    fn push(&mut self, slot_index: usize, priority: u8) {
        debug_assert!(priority as usize <= MAX_PRIORITY);

        self.queues[priority as usize].push_back(slot_index);
        self.non_empty_levels |= 1 << priority;
    }

    fn is_empty(&self) -> bool {
        self.non_empty_levels == 0
    }

    /// The oldest task of the highest priority present
    fn pop_highest_prio_oldest_task(&mut self) -> usize {
        debug_assert!(!self.is_empty());

        let level = (u8::BITS - 1 - self.non_empty_levels.leading_zeros()) as usize;
        let slot_index = self.queues[level].pop_front();
        if self.queues[level].head == INVALID_SLOT {
            self.non_empty_levels &= !(1 << level);
        }

        slot_index
    }

    fn contains(&self, slot_index: usize) -> bool {
        self.queues.iter().any(|queue| queue.contains(slot_index))
    }

    fn take_task_not_currently_on_cpu(&mut self) -> usize {
        for level in (0..PRIORITY_LEVEL_COUNT).rev() {
            let slot_index = self.queues[level]
                .remove_first_where(|queued| !task::slot(queued).on_cpu.load(Ordering::Acquire));
            if slot_index == INVALID_SLOT as usize {
                continue;
            }

            if self.queues[level].head == INVALID_SLOT {
                self.non_empty_levels &= !(1 << level);
            }

            return slot_index;
        }

        INVALID_SLOT as usize
    }
}

/// So every runnable task runs once per epoch and a busy high priority task cannot starve a low one
pub struct CoreRunQueue {
    /// Tasks that still have their turn in this epoch
    executing_tasks: TaskSlotQueueTable,
    /// Tasks that used their slice or yielded, wait until executing_tasks is empty, then queues swap
    yielding_tasks: TaskSlotQueueTable,
    /// Blocked tasks of this core that wait for some point in time
    sleeping_tasks: [SleepingTask; task::MAX_TASKS],
    seeping_tasks_count: usize,
}

#[derive(Clone, Copy)]
struct SleepingTask {
    task_ref: TaskRef,
    wake_time_us: u64,
}

impl SleepingTask {
    const EMPTY: Self = Self {
        task_ref: TaskRef {
            slot: INVALID_SLOT,
            pid: INVALID_PID,
        },
        wake_time_us: 0,
    };

    /// A woken task that was killed or woke up for another reason leaves its entry behind
    fn is_stale(&self) -> bool {
        let task = task::slot(self.task_ref.slot as usize);

        task.pid.load(Ordering::Acquire) != self.task_ref.pid || task.state() != TaskState::Blocked
    }
}

impl CoreRunQueue {
    const fn new() -> Self {
        Self {
            executing_tasks: TaskSlotQueueTable::new(),
            yielding_tasks: TaskSlotQueueTable::new(),
            sleeping_tasks: [SleepingTask::EMPTY; task::MAX_TASKS],
            seeping_tasks_count: 0,
        }
    }

    fn add_sleeping_task(&mut self, task_ref: TaskRef, wake_time_us: u64) {
        if self.seeping_tasks_count == self.sleeping_tasks.len() {
            self.remove_stale_sleeping_tasks();
        }
        debug_assert!(self.seeping_tasks_count < self.sleeping_tasks.len());

        self.sleeping_tasks[self.seeping_tasks_count] = SleepingTask {
            task_ref,
            wake_time_us,
        };
        self.seeping_tasks_count += 1;
    }

    fn remove_stale_sleeping_tasks(&mut self) {
        let mut index = 0;
        while index < self.seeping_tasks_count {
            if self.sleeping_tasks[index].is_stale() {
                self.seeping_tasks_count -= 1;
                self.sleeping_tasks[index] = self.sleeping_tasks[self.seeping_tasks_count];
            } else {
                index += 1;
            }
        }
    }

    /// Returns the highest priority from the awoken tasks, None when nobody woke
    fn wake_due_sleeping_tasks(&mut self, core_id: u8, now_us: u64) -> Option<u8> {
        let mut highest_priority_woken: Option<u8> = None;
        let mut index = 0;

        while index < self.seeping_tasks_count {
            let sleeper = self.sleeping_tasks[index];
            if sleeper.is_stale() {
                self.seeping_tasks_count -= 1;
                self.sleeping_tasks[index] = self.sleeping_tasks[self.seeping_tasks_count];
                continue;
            }

            if sleeper.wake_time_us > now_us {
                index += 1;
                continue;
            }

            let task = task::slot(sleeper.task_ref.slot as usize);
            if task.transition(TaskState::Blocked, TaskState::Ready) {
                let priority = task.priority.load(Ordering::Relaxed);
                RUNNABLE_TASKS_COUNT[core_id as usize].fetch_add(1, Ordering::AcqRel);
                self.executing_tasks
                    .push(sleeper.task_ref.slot as usize, priority);
                highest_priority_woken =
                    Some(highest_priority_woken.map_or(priority, |p| p.max(priority)));
            }

            self.seeping_tasks_count -= 1;
            self.sleeping_tasks[index] = self.sleeping_tasks[self.seeping_tasks_count];
        }

        highest_priority_woken
    }

    fn push_executing_task(&mut self, slot_index: usize, priority: u8) {
        self.executing_tasks.push(slot_index, priority);
    }

    fn push_yielding_task(&mut self, slot_index: usize, priority: u8) {
        self.yielding_tasks.push(slot_index, priority);
    }

    /// INVALID_SLOT when nothing is queued
    fn pick_next_task(&mut self) -> usize {
        if self.executing_tasks.is_empty() {
            if self.yielding_tasks.is_empty() {
                return INVALID_SLOT as usize;
            }

            core::mem::swap(&mut self.executing_tasks, &mut self.yielding_tasks);
        }

        self.executing_tasks.pop_highest_prio_oldest_task()
    }

    /// A queued task another core may run, INVALID_SLOT when there is none
    fn take_stealable_task(&mut self) -> usize {
        let slot_index = self.executing_tasks.take_task_not_currently_on_cpu();
        if slot_index != INVALID_SLOT as usize {
            return slot_index;
        }

        self.yielding_tasks.take_task_not_currently_on_cpu()
    }

    fn contains(&self, slot_index: usize) -> bool {
        self.executing_tasks.contains(slot_index) || self.yielding_tasks.contains(slot_index)
    }
}

static CORE_RUN_QUEUES: [CacheAligned<IrqMutex<CoreRunQueue>>; MAX_CORES as usize] =
    [const { CacheAligned(IrqMutex::new(CoreRunQueue::new())) }; MAX_CORES as usize];

fn set_current_task_for_core(core_id: u8, slot_index: u16, pid: PID) {
    CURRENT_SLOT_ON_CORE[core_id as usize].store(slot_index, Ordering::Release);
    CURRENT_PROCESS_ON_CORE[core_id as usize].store(pid as u64, Ordering::Release);
}

pub fn get_current_process_for_core(core_id: u8) -> PID {
    CURRENT_PROCESS_ON_CORE[core_id as usize].load(Ordering::Acquire) as PID
}

pub fn get_current_slot_for_core(core_id: u8) -> usize {
    CURRENT_SLOT_ON_CORE[core_id as usize].load(Ordering::Acquire) as usize
}

pub fn core_runnable_tasks_count(core_id: u8) -> usize {
    RUNNABLE_TASKS_COUNT[core_id as usize].load(Ordering::Acquire) as usize
}

pub fn core_task_leave_run_queue(core_id: u8) {
    let previous = RUNNABLE_TASKS_COUNT[core_id as usize].fetch_sub(1, Ordering::AcqRel);
    debug_assert!(previous > 0);
}

fn get_slice_ticks_for_priority(priority: u8) -> u32 {
    (priority as u32 + 1) * BASE_SLICE_TICKS
}

pub fn mark_core_for_reschedule(core_id: u8) {
    CORE_SHOULD_BE_RESCHEDULED[core_id as usize].store(true, Ordering::Release);
}

pub fn on_timer_tick(core_id: u8) {
    wake_due_sleeping_tasks(core_id);

    let slot_index = get_current_slot_for_core(core_id);
    if slot_index == INVALID_SLOT as usize {
        return;
    }

    let task = task::slot(slot_index);
    task.cpu_ticks.fetch_add(1, Ordering::Relaxed);

    let remaining_ticks = SLICE_REMAINING_TICKS[core_id as usize].load(Ordering::Relaxed);
    if remaining_ticks > 1 {
        SLICE_REMAINING_TICKS[core_id as usize].store(remaining_ticks - 1, Ordering::Relaxed);
        return;
    }

    let another_task_waits = RUNNABLE_TASKS_COUNT[core_id as usize].load(Ordering::Acquire) > 1;
    if another_task_waits {
        SLICE_REMAINING_TICKS[core_id as usize].store(0, Ordering::Relaxed);
        mark_core_for_reschedule(core_id);
    } else {
        let priority = task.priority.load(Ordering::Relaxed);
        SLICE_REMAINING_TICKS[core_id as usize]
            .store(get_slice_ticks_for_priority(priority), Ordering::Relaxed);
    }
}

fn wake_due_sleeping_tasks(core_id: u8) {
    let now_us = crate::interrupts::tsc_timestamp_us();
    let highest_priority_woken = CORE_RUN_QUEUES[core_id as usize]
        .0
        .lock()
        .wake_due_sleeping_tasks(core_id, now_us);

    let Some(woken_priority) = highest_priority_woken else {
        return;
    };

    // an idle core picks the woken task up by itself when the interrupt returns
    let running_slot = get_current_slot_for_core(core_id);
    if running_slot != INVALID_SLOT as usize
        && task::slot(running_slot).priority.load(Ordering::Relaxed) < woken_priority
    {
        mark_core_for_reschedule(core_id);
    }
}

pub fn core_block_current_task_until_wake_time(wake_time_us: u64, core_id: u8) {
    let task_ref = task::core_current_task_ref(core_id);
    let task = task::slot(task_ref.slot as usize);
    let home_core = task.home_core.load(Ordering::Relaxed);

    {
        let mut run_queue = CORE_RUN_QUEUES[home_core as usize].0.lock();
        run_queue.add_sleeping_task(task_ref, wake_time_us);

        let was_running = task.transition(TaskState::Running, TaskState::Blocked);
        debug_assert!(was_running);
        core_task_leave_run_queue(home_core);
    }

    core_run_scheduling(home_core);
}

pub fn core_enqueue_ready_task(core_id: u8, slot_index: usize, priority: u8) {
    RUNNABLE_TASKS_COUNT[core_id as usize].fetch_add(1, Ordering::AcqRel);
    CORE_RUN_QUEUES[core_id as usize]
        .0
        .lock()
        .push_executing_task(slot_index, priority);

    let running_slot = get_current_slot_for_core(core_id);
    let should_preempt = running_slot == INVALID_SLOT as usize
        || task::slot(running_slot).priority.load(Ordering::Relaxed) < priority;
    if !should_preempt {
        return;
    }

    if core_id == get_current_core_id() {
        mark_core_for_reschedule(core_id);
    } else {
        crate::interrupts::send_reschedule_ipi(core_id);
    }
}

pub fn is_pid_queued(pid: PID) -> bool {
    let Some(slot_index) = task::find_slot_by_pid(pid) else {
        return false;
    };

    for core_id in 0..MAX_CORES {
        if CORE_RUN_QUEUES[core_id as usize]
            .0
            .lock()
            .contains(slot_index)
        {
            return true;
        }
    }

    (0..MAX_CORES).any(|core_id| get_current_slot_for_core(core_id) == slot_index)
}

/// Last stop before a core returns to ring 3, with interrupts off
/// Runs the pending reschedules, so the task comes back here when it is its turn again
pub fn exit_to_user() {
    debug_assert!(!interrupts::are_enabled());

    loop {
        task::exit_if_kill_pending();

        let core_id = get_current_core_id();
        if !CORE_SHOULD_BE_RESCHEDULED[core_id as usize].swap(false, Ordering::AcqRel) {
            return;
        }

        core_run_scheduling(core_id);
    }
}

pub fn wake_task(task_ref: TaskRef) {
    let task = task::slot(task_ref.slot as usize);
    if task.pid.load(Ordering::Acquire) != task_ref.pid {
        return;
    }

    if !task.transition(TaskState::Blocked, TaskState::Ready) {
        return;
    }

    // blocked tasks do not migrate, so the home core is the one that last ran it
    let home_core = task.home_core.load(Ordering::Relaxed);
    let priority = task.priority.load(Ordering::Relaxed);
    core_enqueue_ready_task(home_core, task_ref.slot as usize, priority);
}

/// Saves the callee-saved registers on the current stack, stores the stack pointer
/// to `save_rsp`, then continues on `load_rsp`
#[unsafe(naked)]
pub unsafe extern "C" fn switch_context(save_rsp: *mut u64, load_rsp: u64) {
    naked_asm!(
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov [rdi], rsp",
        "mov rsp, rsi",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",
        "ret",
    )
}

pub fn run_on_core_loop(core_id: u8) -> ! {
    // SAFETY: the stack top belongs to this core and nothing runs on it yet
    unsafe { enter_scheduler_stack(core_id as u64, crate::gdt::get_scheduler_stack_top(core_id)) }
}

#[unsafe(naked)]
unsafe extern "C" fn enter_scheduler_stack(core_id: u64, stack_top: u64) -> ! {
    naked_asm!(
        "mov rsp, rsi",
        "xor ebp, ebp",
        "call {scheduler_loop}",
        "ud2",
        scheduler_loop = sym scheduler_loop,
    )
}

extern "C" fn scheduler_loop(core_id: u64) -> ! {
    let core_id = core_id as u8;
    debug_assert!(get_current_core_id() == core_id);

    if SCHED_DEBUG_PRINT {
        serial_println_core!("Entering scheduler loop (core_id={})", core_id);
    }

    let mut kernel_cr3_loaded = false;

    loop {
        interrupts::disable();

        let slot_index = CORE_RUN_QUEUES[core_id as usize].0.lock().pick_next_task();

        if slot_index == INVALID_SLOT as usize {
            if WORK_STEALING_ENABLED && try_steal_task(core_id) {
                continue;
            }

            if !kernel_cr3_loaded {
                let kernel_page_table_phys =
                    crate::memory::get_user_mem_mgr().kernel_page_table_phys;
                let frame = PhysFrame::containing_address(kernel_page_table_phys);
                unsafe { Cr3::write(frame, Cr3Flags::empty()) };
                kernel_cr3_loaded = true;
            }

            interrupts::enable_and_hlt();
            continue;
        }

        run_task(core_id, slot_index);
        kernel_cr3_loaded = false;
    }
}

/// Moves one waiting task from a busy core to the core_id one
fn try_steal_task(core_id: u8) -> bool {
    for core_from in 1..MAX_CORES {
        if core_from == core_id {
            continue;
        }

        if RUNNABLE_TASKS_COUNT[core_from as usize].load(Ordering::Acquire) <= 1 {
            continue;
        }

        let Some(mut core_from_queue) = CORE_RUN_QUEUES[core_from as usize].0.try_lock() else {
            continue;
        };
        let slot_index = core_from_queue.take_stealable_task();
        drop(core_from_queue);

        if slot_index == INVALID_SLOT as usize {
            continue;
        }

        let task = task::slot(slot_index);
        task.home_core.store(core_id, Ordering::Relaxed);
        task.core_migration_count.fetch_add(1, Ordering::Relaxed);
        RUNNABLE_TASKS_COUNT[core_from as usize].fetch_sub(1, Ordering::AcqRel);
        RUNNABLE_TASKS_COUNT[core_id as usize].fetch_add(1, Ordering::AcqRel);

        let priority = task.priority.load(Ordering::Relaxed);
        CORE_RUN_QUEUES[core_id as usize]
            .0
            .lock()
            .push_executing_task(slot_index, priority);

        if SCHED_DEBUG_PRINT {
            serial_println_core!("Stole slot {} from core {}", slot_index, core_from);
        }

        return true;
    }

    false
}

fn run_task(core_id: u8, slot_index: usize) {
    let task = task::slot(slot_index);
    debug_assert!(task.state() == TaskState::Ready);

    let pid = task.pid.load(Ordering::Acquire);
    task.set_state(TaskState::Running);
    task.on_cpu.store(true, Ordering::Release);
    set_current_task_for_core(core_id, slot_index as u16, pid);

    CORE_SHOULD_BE_RESCHEDULED[core_id as usize].store(false, Ordering::Release);
    SLICE_REMAINING_TICKS[core_id as usize].store(
        get_slice_ticks_for_priority(task.priority.load(Ordering::Relaxed)),
        Ordering::Relaxed,
    );

    // ring 3 entries from here on land on the stack of this task
    let stack_top = task.kernel_stack_top.load(Ordering::Relaxed);
    crate::process::syscall::set_syscall_stack_top(core_id, stack_top);
    crate::gdt::set_privilege_stack_top(core_id, stack_top);

    if SCHED_DEBUG_PRINT {
        serial_println_core!("Running PID {} from slot {}", pid, slot_index);
    }

    // the kernel has no xmm code, so this state stays live until the return to ring 3
    unsafe {
        (*task.fpu_state.get()).restore_to_cpu();

        let frame = PhysFrame::containing_address(PhysAddr::new(task.cr3.load(Ordering::Relaxed)));
        Cr3::write(frame, Cr3Flags::empty());

        switch_context(
            core::ptr::addr_of_mut!(SCHEDULER_RSP[core_id as usize]),
            *task.saved_rsp.get(),
        );
    }

    // the task switched back to this loop, its stack is no longer in use
    task.on_cpu.store(false, Ordering::Release);
    set_current_task_for_core(core_id, INVALID_SLOT, INVALID_PID);

    if task.state() == TaskState::Dead {
        if SCHED_DEBUG_PRINT {
            serial_println_core!("PID {} dead, releasing slot {}", pid, slot_index);
        }
        task::release_task_slot(slot_index);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RequeueType {
    ExecutingTasks,
    YieldingTasks,
}

pub fn core_run_scheduling(core_id: u8) {
    let requeue_type = if SLICE_REMAINING_TICKS[core_id as usize].load(Ordering::Relaxed) == 0 {
        RequeueType::YieldingTasks
    } else {
        RequeueType::ExecutingTasks
    };

    core_switch_executing_yielding(core_id, requeue_type);
}

pub fn core_yield_executing_tasks(core_id: u8) {
    if RUNNABLE_TASKS_COUNT[core_id as usize].load(Ordering::Acquire) <= 1 {
        return;
    }

    core_switch_executing_yielding(core_id, RequeueType::YieldingTasks);
}

fn core_switch_executing_yielding(core_id: u8, requeue_type: RequeueType) {
    let interrupts_were_enabled = interrupts::are_enabled();
    interrupts::disable();

    let slot_index = get_current_slot_for_core(core_id);
    debug_assert!(slot_index != INVALID_SLOT as usize);

    let task = task::slot(slot_index);

    unsafe { (*task.fpu_state.get()).save_from_cpu() };

    // blocked and dead tasks are not queued again
    if task.transition(TaskState::Running, TaskState::Ready) {
        let priority = task.priority.load(Ordering::Relaxed);
        debug_assert!(task.home_core.load(Ordering::Relaxed) == core_id);
        let mut run_queue = CORE_RUN_QUEUES[core_id as usize].0.lock();
        match requeue_type {
            RequeueType::ExecutingTasks => run_queue.push_executing_task(slot_index, priority),
            RequeueType::YieldingTasks => run_queue.push_yielding_task(slot_index, priority),
        }
    }

    unsafe {
        switch_context(
            task.saved_rsp.get(),
            *core::ptr::addr_of!(SCHEDULER_RSP[core_id as usize]),
        );
    }

    if interrupts_were_enabled {
        interrupts::enable();
    }
}
