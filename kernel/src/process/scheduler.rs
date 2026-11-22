use crate::process::INVALID_PID;
use crate::process::PID;
use crate::process::thread::{self, INVALID_SLOT, ThreadRef, ThreadState};
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

/// An idle core takes a waiting thread from the run queue of a busy core
const WORK_STEALING_ENABLED: bool = true;

// note non_empty_levels is u8 right now, increase size if we ever increase max prio, but why
pub const MAX_PRIORITY: usize = 7;
const PRIORITY_LEVEL_COUNT: usize = MAX_PRIORITY + 1;
const _: () = assert!(PRIORITY_LEVEL_COUNT <= u8::BITS as usize);

/// Timer ticks a thread of priority 0 runs before it has to give way
const BASE_SLICE_TICKS: u32 = 1;

static CURRENT_PROCESS_ON_CORE: [AtomicU64; MAX_CORES as usize] =
    [const { AtomicU64::new(INVALID_PID as u64) }; MAX_CORES as usize];

static CURRENT_SLOT_ON_CORE: [AtomicU16; MAX_CORES as usize] =
    [const { AtomicU16::new(INVALID_SLOT) }; MAX_CORES as usize];

/// PID of the thread that ran last on each core, only the owning core touches it
static LAST_PROCESS_ON_CORE: [AtomicU64; MAX_CORES as usize] =
    [const { AtomicU64::new(INVALID_PID as u64) }; MAX_CORES as usize];

/// Set when the thread running on a core should call core_run_scheduling() before it returns to ring 3
static CORE_SHOULD_BE_RESCHEDULED: [AtomicBool; MAX_CORES as usize] =
    [const { AtomicBool::new(false) }; MAX_CORES as usize];

static SLICE_REMAINING_TICKS: [AtomicU32; MAX_CORES as usize] =
    [const { AtomicU32::new(0) }; MAX_CORES as usize];

/// Threads that are queued or running on each core, readable from interrupt context without a queue lock
static RUNNABLE_THREADS_COUNT: [AtomicU32; MAX_CORES as usize] =
    [const { AtomicU32::new(0) }; MAX_CORES as usize];

/// Stack pointer of each core's scheduler loop while a thread runs
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
        let thread = thread::slot(slot_index);
        thread.next.store(INVALID_SLOT, Ordering::Relaxed);

        if self.tail == INVALID_SLOT {
            self.head = slot_index as u16;
        } else {
            thread::slot(self.tail as usize)
                .next
                .store(slot_index as u16, Ordering::Relaxed);
        }
        self.tail = slot_index as u16;
    }

    fn pop_front(&mut self) -> usize {
        debug_assert!(self.head != INVALID_SLOT);

        let slot_index = self.head as usize;
        let next = thread::slot(slot_index).next.load(Ordering::Relaxed);
        self.head = next;
        if next == INVALID_SLOT {
            self.tail = INVALID_SLOT;
        }

        slot_index
    }

    /// Unlinks the first thread the predicate accepts, INVALID_SLOT when none does
    fn remove_first_where(&mut self, accepts: impl Fn(usize) -> bool) -> usize {
        let mut previous = INVALID_SLOT;
        let mut cursor = self.head;

        while cursor != INVALID_SLOT {
            let thread = thread::slot(cursor as usize);
            let next = thread.next.load(Ordering::Relaxed);

            if accepts(cursor as usize) {
                if previous == INVALID_SLOT {
                    self.head = next;
                } else {
                    thread::slot(previous as usize)
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
            cursor = thread::slot(cursor as usize).next.load(Ordering::Relaxed);
        }

        false
    }
}

struct ThreadSlotQueueTable {
    queues: [SlotQueue; PRIORITY_LEVEL_COUNT],
    non_empty_levels: u8, // bitmap
}

impl ThreadSlotQueueTable {
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

    /// The oldest thread of the highest priority present
    fn pop_highest_prio_oldest_thread(&mut self) -> usize {
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

    fn take_thread_not_currently_on_cpu(&mut self) -> usize {
        for level in (0..PRIORITY_LEVEL_COUNT).rev() {
            let slot_index = self.queues[level]
                .remove_first_where(|queued| !thread::slot(queued).on_cpu.load(Ordering::Acquire));
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

/// So every runnable thread runs once per epoch and a busy high priority thread cannot starve a low one
pub struct CoreRunQueue {
    /// Threads that still have their turn in this epoch
    executing_threads: ThreadSlotQueueTable,
    /// Threads that used their slice or yielded, wait until executing_threads is empty, then queues swap
    yielding_threads: ThreadSlotQueueTable,
    /// Blocked threads of this core that wait for some point in time
    sleeping_threads: [SleepingThread; thread::MAX_THREADS],
    seeping_threads_count: usize,
}

#[derive(Clone, Copy)]
struct SleepingThread {
    thread_ref: ThreadRef,
    wake_time_us: u64,
}

impl SleepingThread {
    const EMPTY: Self = Self {
        thread_ref: ThreadRef {
            slot: INVALID_SLOT,
            pid: INVALID_PID,
        },
        wake_time_us: 0,
    };

    /// A woken thread that was killed or woke up for another reason leaves its entry behind
    fn is_stale(&self) -> bool {
        let thread = thread::slot(self.thread_ref.slot as usize);

        thread.pid.load(Ordering::Acquire) != self.thread_ref.pid
            || thread.state() != ThreadState::Blocked
    }
}

impl CoreRunQueue {
    const fn new() -> Self {
        Self {
            executing_threads: ThreadSlotQueueTable::new(),
            yielding_threads: ThreadSlotQueueTable::new(),
            sleeping_threads: [SleepingThread::EMPTY; thread::MAX_THREADS],
            seeping_threads_count: 0,
        }
    }

    fn add_sleeping_thread(&mut self, thread_ref: ThreadRef, wake_time_us: u64) {
        if self.seeping_threads_count == self.sleeping_threads.len() {
            self.remove_stale_sleeping_threads();
        }
        debug_assert!(self.seeping_threads_count < self.sleeping_threads.len());

        self.sleeping_threads[self.seeping_threads_count] = SleepingThread {
            thread_ref,
            wake_time_us,
        };
        self.seeping_threads_count += 1;
    }

    fn remove_stale_sleeping_threads(&mut self) {
        let mut index = 0;
        while index < self.seeping_threads_count {
            if self.sleeping_threads[index].is_stale() {
                self.seeping_threads_count -= 1;
                self.sleeping_threads[index] = self.sleeping_threads[self.seeping_threads_count];
            } else {
                index += 1;
            }
        }
    }

    /// Returns the highest priority from the awoken threads, None when nobody woke
    fn wake_due_sleeping_threads(&mut self, core_id: u8, now_us: u64) -> Option<u8> {
        let mut highest_priority_woken: Option<u8> = None;
        let mut index = 0;

        while index < self.seeping_threads_count {
            let sleeper = self.sleeping_threads[index];
            if sleeper.is_stale() {
                self.seeping_threads_count -= 1;
                self.sleeping_threads[index] = self.sleeping_threads[self.seeping_threads_count];
                continue;
            }

            if sleeper.wake_time_us > now_us {
                index += 1;
                continue;
            }

            let thread = thread::slot(sleeper.thread_ref.slot as usize);
            if thread.transition(ThreadState::Blocked, ThreadState::Ready) {
                let priority = thread.priority.load(Ordering::Relaxed);
                RUNNABLE_THREADS_COUNT[core_id as usize].fetch_add(1, Ordering::AcqRel);
                self.executing_threads
                    .push(sleeper.thread_ref.slot as usize, priority);
                highest_priority_woken =
                    Some(highest_priority_woken.map_or(priority, |p| p.max(priority)));
            }

            self.seeping_threads_count -= 1;
            self.sleeping_threads[index] = self.sleeping_threads[self.seeping_threads_count];
        }

        highest_priority_woken
    }

    fn push_executing_thread(&mut self, slot_index: usize, priority: u8) {
        self.executing_threads.push(slot_index, priority);
    }

    fn push_yielding_thread(&mut self, slot_index: usize, priority: u8) {
        self.yielding_threads.push(slot_index, priority);
    }

    /// INVALID_SLOT when nothing is queued
    fn pick_next_thread(&mut self) -> usize {
        if self.executing_threads.is_empty() {
            if self.yielding_threads.is_empty() {
                return INVALID_SLOT as usize;
            }

            core::mem::swap(&mut self.executing_threads, &mut self.yielding_threads);
        }

        self.executing_threads.pop_highest_prio_oldest_thread()
    }

    /// A queued thread another core may run, INVALID_SLOT when there is none
    fn take_stealable_thread(&mut self) -> usize {
        let slot_index = self.executing_threads.take_thread_not_currently_on_cpu();
        if slot_index != INVALID_SLOT as usize {
            return slot_index;
        }

        self.yielding_threads.take_thread_not_currently_on_cpu()
    }

    fn contains(&self, slot_index: usize) -> bool {
        self.executing_threads.contains(slot_index) || self.yielding_threads.contains(slot_index)
    }
}

static CORE_RUN_QUEUES: [CacheAligned<IrqMutex<CoreRunQueue>>; MAX_CORES as usize] =
    [const { CacheAligned(IrqMutex::new(CoreRunQueue::new())) }; MAX_CORES as usize];

fn set_current_thread_for_core(core_id: u8, slot_index: u16, pid: PID) {
    CURRENT_SLOT_ON_CORE[core_id as usize].store(slot_index, Ordering::Release);
    CURRENT_PROCESS_ON_CORE[core_id as usize].store(pid as u64, Ordering::Release);
}

pub fn get_current_process_for_core(core_id: u8) -> PID {
    CURRENT_PROCESS_ON_CORE[core_id as usize].load(Ordering::Acquire) as PID
}

pub fn get_current_slot_for_core(core_id: u8) -> usize {
    CURRENT_SLOT_ON_CORE[core_id as usize].load(Ordering::Acquire) as usize
}

pub fn core_runnable_threads_count(core_id: u8) -> usize {
    RUNNABLE_THREADS_COUNT[core_id as usize].load(Ordering::Acquire) as usize
}

pub fn core_thread_leave_run_queue(core_id: u8) {
    let previous = RUNNABLE_THREADS_COUNT[core_id as usize].fetch_sub(1, Ordering::AcqRel);
    debug_assert!(previous > 0);
}

fn get_slice_ticks_for_priority(priority: u8) -> u32 {
    (priority as u32 + 1) * BASE_SLICE_TICKS
}

pub fn mark_core_for_reschedule(core_id: u8) {
    CORE_SHOULD_BE_RESCHEDULED[core_id as usize].store(true, Ordering::Release);
}

pub fn on_timer_tick(core_id: u8) {
    wake_due_sleeping_threads(core_id);

    let slot_index = get_current_slot_for_core(core_id);
    if slot_index == INVALID_SLOT as usize {
        return;
    }

    let thread = thread::slot(slot_index);
    thread.cpu_ticks.fetch_add(1, Ordering::Relaxed);

    let remaining_ticks = SLICE_REMAINING_TICKS[core_id as usize].load(Ordering::Relaxed);
    if remaining_ticks > 1 {
        SLICE_REMAINING_TICKS[core_id as usize].store(remaining_ticks - 1, Ordering::Relaxed);
        return;
    }

    let another_thread_waits = RUNNABLE_THREADS_COUNT[core_id as usize].load(Ordering::Acquire) > 1;
    if another_thread_waits {
        SLICE_REMAINING_TICKS[core_id as usize].store(0, Ordering::Relaxed);
        mark_core_for_reschedule(core_id);
    } else {
        let priority = thread.priority.load(Ordering::Relaxed);
        SLICE_REMAINING_TICKS[core_id as usize]
            .store(get_slice_ticks_for_priority(priority), Ordering::Relaxed);
    }
}

fn wake_due_sleeping_threads(core_id: u8) {
    let now_us = crate::interrupts::tsc_timestamp_us();
    let highest_priority_woken = CORE_RUN_QUEUES[core_id as usize]
        .0
        .lock()
        .wake_due_sleeping_threads(core_id, now_us);

    let Some(woken_priority) = highest_priority_woken else {
        return;
    };

    // an idle core picks the woken thread up by itself when the interrupt returns
    let running_slot = get_current_slot_for_core(core_id);
    if running_slot != INVALID_SLOT as usize
        && thread::slot(running_slot).priority.load(Ordering::Relaxed) < woken_priority
    {
        mark_core_for_reschedule(core_id);
    }
}

pub fn core_block_current_thread_until_wake_time(wake_time_us: u64, core_id: u8) {
    let thread_ref = thread::core_current_thread_ref(core_id);
    let thread = thread::slot(thread_ref.slot as usize);
    let assigned_core = thread.assigned_core.load(Ordering::Relaxed);

    {
        let mut run_queue = CORE_RUN_QUEUES[assigned_core as usize].0.lock();
        run_queue.add_sleeping_thread(thread_ref, wake_time_us);

        let was_running = thread.transition(ThreadState::Running, ThreadState::Blocked);
        debug_assert!(was_running);
        core_thread_leave_run_queue(assigned_core);
    }

    core_run_scheduling(assigned_core);
}

pub fn core_enqueue_ready_thread(core_id: u8, slot_index: usize, priority: u8) {
    RUNNABLE_THREADS_COUNT[core_id as usize].fetch_add(1, Ordering::AcqRel);
    CORE_RUN_QUEUES[core_id as usize]
        .0
        .lock()
        .push_executing_thread(slot_index, priority);

    let running_slot = get_current_slot_for_core(core_id);
    let should_preempt = running_slot == INVALID_SLOT as usize
        || thread::slot(running_slot).priority.load(Ordering::Relaxed) < priority;
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
    let Some(slot_index) = thread::find_slot_by_pid(pid) else {
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
/// Runs the pending reschedules, so the thread comes back here when it is its turn again
pub fn exit_to_user() {
    debug_assert!(!interrupts::are_enabled());

    loop {
        thread::exit_if_kill_pending();

        let core_id = get_current_core_id();
        if !CORE_SHOULD_BE_RESCHEDULED[core_id as usize].swap(false, Ordering::AcqRel) {
            return;
        }

        core_run_scheduling(core_id);
    }
}

pub fn wake_thread(thread_ref: ThreadRef) {
    let thread = thread::slot(thread_ref.slot as usize);
    if thread.pid.load(Ordering::Acquire) != thread_ref.pid {
        return;
    }

    if !thread.transition(ThreadState::Blocked, ThreadState::Ready) {
        return;
    }

    // blocked threads do not migrate, so the running core is the one that last ran it
    let assigned_core = thread.assigned_core.load(Ordering::Relaxed);
    let priority = thread.priority.load(Ordering::Relaxed);
    core_enqueue_ready_thread(assigned_core, thread_ref.slot as usize, priority);
}

/// Saves the callee-saved registers on the current stack, stores the stack pointer
/// to `save_rsp`, then continues on `load_rsp`
///
/// # Safety
///
/// `save_rsp` must be valid for writes, and `load_rsp` must be a stack pointer
/// saved by `switch_context` or a stack prepared to look like one.
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

        let slot_index = CORE_RUN_QUEUES[core_id as usize]
            .0
            .lock()
            .pick_next_thread();

        if slot_index == INVALID_SLOT as usize {
            if WORK_STEALING_ENABLED && try_steal_thread(core_id) {
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

        run_thread(core_id, slot_index);
        kernel_cr3_loaded = false;
    }
}

/// Moves one waiting thread from a busy core to the core_id one
fn try_steal_thread(core_id: u8) -> bool {
    for core_from in 1..MAX_CORES {
        if core_from == core_id {
            continue;
        }

        if RUNNABLE_THREADS_COUNT[core_from as usize].load(Ordering::Acquire) <= 1 {
            continue;
        }

        let Some(mut core_from_queue) = CORE_RUN_QUEUES[core_from as usize].0.try_lock() else {
            continue;
        };
        let slot_index = core_from_queue.take_stealable_thread();
        drop(core_from_queue);

        if slot_index == INVALID_SLOT as usize {
            continue;
        }

        let thread = thread::slot(slot_index);
        thread.assigned_core.store(core_id, Ordering::Relaxed);
        thread.core_migration_count.fetch_add(1, Ordering::Relaxed);
        RUNNABLE_THREADS_COUNT[core_from as usize].fetch_sub(1, Ordering::AcqRel);
        RUNNABLE_THREADS_COUNT[core_id as usize].fetch_add(1, Ordering::AcqRel);

        let priority = thread.priority.load(Ordering::Relaxed);
        CORE_RUN_QUEUES[core_id as usize]
            .0
            .lock()
            .push_executing_thread(slot_index, priority);

        if SCHED_DEBUG_PRINT {
            serial_println_core!("Stole slot {} from core {}", slot_index, core_from);
        }

        return true;
    }

    false
}

fn run_thread(core_id: u8, slot_index: usize) {
    let thread = thread::slot(slot_index);
    debug_assert!(thread.state() == ThreadState::Ready);

    let pid = thread.pid.load(Ordering::Acquire);
    let last_pid = LAST_PROCESS_ON_CORE[core_id as usize].swap(pid as u64, Ordering::Relaxed);
    if last_pid != pid as u64 {
        thread.handoff_count.fetch_add(1, Ordering::Relaxed);
    }

    thread.set_state(ThreadState::Running);
    thread.on_cpu.store(true, Ordering::Release);
    set_current_thread_for_core(core_id, slot_index as u16, pid);

    CORE_SHOULD_BE_RESCHEDULED[core_id as usize].store(false, Ordering::Release);
    SLICE_REMAINING_TICKS[core_id as usize].store(
        get_slice_ticks_for_priority(thread.priority.load(Ordering::Relaxed)),
        Ordering::Relaxed,
    );

    // ring 3 entries from here on land on the stack of this thread
    let stack_top = thread.kernel_stack_top.load(Ordering::Relaxed);
    crate::process::syscall::set_syscall_stack_top(core_id, stack_top);
    crate::gdt::set_privilege_stack_top(core_id, stack_top);

    if SCHED_DEBUG_PRINT {
        serial_println_core!("Running PID {} from slot {}", pid, slot_index);
    }

    // the kernel has no xmm code, so this state stays live until the return to ring 3
    unsafe {
        (*thread.fpu_state.get()).restore_to_cpu();

        let frame =
            PhysFrame::containing_address(PhysAddr::new(thread.cr3.load(Ordering::Relaxed)));
        Cr3::write(frame, Cr3Flags::empty());

        switch_context(
            core::ptr::addr_of_mut!(SCHEDULER_RSP[core_id as usize]),
            *thread.saved_rsp.get(),
        );
    }

    // the thread switched back to this loop, its stack is no longer in use
    thread.on_cpu.store(false, Ordering::Release);
    set_current_thread_for_core(core_id, INVALID_SLOT, INVALID_PID);

    if thread.state() == ThreadState::Dead {
        if SCHED_DEBUG_PRINT {
            serial_println_core!("PID {} dead, releasing slot {}", pid, slot_index);
        }
        thread::release_thread_slot(slot_index);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RequeueType {
    ExecutingThreads,
    YieldingThreads,
}

pub fn core_run_scheduling(core_id: u8) {
    let requeue_type = if SLICE_REMAINING_TICKS[core_id as usize].load(Ordering::Relaxed) == 0 {
        RequeueType::YieldingThreads
    } else {
        RequeueType::ExecutingThreads
    };

    core_switch_executing_yielding(core_id, requeue_type);
}

pub fn core_yield_executing_threads(core_id: u8) {
    if RUNNABLE_THREADS_COUNT[core_id as usize].load(Ordering::Acquire) <= 1 {
        return;
    }

    core_switch_executing_yielding(core_id, RequeueType::YieldingThreads);
}

fn core_switch_executing_yielding(core_id: u8, requeue_type: RequeueType) {
    let interrupts_were_enabled = interrupts::are_enabled();
    interrupts::disable();

    let slot_index = get_current_slot_for_core(core_id);
    debug_assert!(slot_index != INVALID_SLOT as usize);

    let thread = thread::slot(slot_index);

    unsafe { (*thread.fpu_state.get()).save_from_cpu() };

    // blocked and dead threads are not queued again
    if thread.transition(ThreadState::Running, ThreadState::Ready) {
        let priority = thread.priority.load(Ordering::Relaxed);
        debug_assert!(thread.assigned_core.load(Ordering::Relaxed) == core_id);
        let mut run_queue = CORE_RUN_QUEUES[core_id as usize].0.lock();
        match requeue_type {
            RequeueType::ExecutingThreads => run_queue.push_executing_thread(slot_index, priority),
            RequeueType::YieldingThreads => run_queue.push_yielding_thread(slot_index, priority),
        }
    }

    unsafe {
        switch_context(
            thread.saved_rsp.get(),
            *core::ptr::addr_of!(SCHEDULER_RSP[core_id as usize]),
        );
    }

    if interrupts_were_enabled {
        interrupts::enable();
    }
}
