use crate::process::scheduler;
use crate::process::task::{self, MAX_TASKS, TaskRef, TaskState};
use crate::util::cpuinfo::get_current_core_id;
use crate::util::irq_mutex::IrqMutex;
use core::sync::atomic::Ordering;

const MICROSECONDS_PER_MILLISECOND: u64 = 1000;

struct WaiterList {
    waiters: [TaskRef; MAX_TASKS],
    count: usize,
}

impl WaiterList {
    const fn new() -> Self {
        Self {
            waiters: [TaskRef {
                slot: task::INVALID_SLOT,
                pid: crate::process::process::INVALID_PID,
            }; MAX_TASKS],
            count: 0,
        }
    }

    fn is_stale(task_ref: TaskRef) -> bool {
        let task = task::slot(task_ref.slot as usize);

        task.pid.load(Ordering::Acquire) != task_ref.pid || task.state() != TaskState::Blocked
    }

    fn push(&mut self, task_ref: TaskRef) {
        if self.count == self.waiters.len() {
            self.remove_stale();
        }
        debug_assert!(self.count < self.waiters.len());

        self.waiters[self.count] = task_ref;
        self.count += 1;
    }

    /// Tasks killed while they waited leave their entry behind
    fn remove_stale(&mut self) {
        let mut index = 0;
        while index < self.count {
            if Self::is_stale(self.waiters[index]) {
                self.count -= 1;
                self.waiters[index] = self.waiters[self.count];
            } else {
                index += 1;
            }
        }
    }

    fn remove(&mut self, task_ref: TaskRef) {
        if let Some(index) = self.waiters[..self.count]
            .iter()
            .position(|waiter| *waiter == task_ref)
        {
            self.count -= 1;
            self.waiters[index] = self.waiters[self.count];
        }
    }
}

/// Tasks that wait for something to happen, woken by whoever makes it happen
/// Safe to use from interrupt context
pub struct WaitQueue {
    list: IrqMutex<WaiterList>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        Self {
            list: IrqMutex::new(WaiterList::new()),
        }
    }

    pub fn add_current_task_and_mark_blocked(&self) {
        let core_id = get_current_core_id();
        let task_ref = task::core_current_task_ref(core_id);
        let task = task::slot(task_ref.slot as usize);

        let mut list = self.list.lock();
        list.push(task_ref);

        let was_running = task.transition(TaskState::Running, TaskState::Blocked);
        debug_assert!(was_running);
        scheduler::core_task_leave_run_queue(task.home_core.load(Ordering::Relaxed));
    }

    pub fn remove(&self, task_ref: TaskRef) {
        self.list.lock().remove(task_ref);
    }

    pub fn wake_all(&self) {
        // wake_task takes a run queue lock, so no waiter list lock is held while it runs
        let mut woken = [TaskRef {
            slot: task::INVALID_SLOT,
            pid: crate::process::process::INVALID_PID,
        }; MAX_TASKS];
        let woken_count = {
            let mut list = self.list.lock();
            let count = list.count;
            woken[..count].copy_from_slice(&list.waiters[..count]);
            list.count = 0;
            count
        };

        for task_ref in &woken[..woken_count] {
            scheduler::wake_task(*task_ref);
        }
    }
}

pub fn block_current_task_until_event(queue: &WaitQueue, condition: impl Fn() -> bool) {
    let core_id = get_current_core_id();
    loop {
        if condition() || task::core_current_task_kill_pending(core_id) {
            return;
        }

        queue.add_current_task_and_mark_blocked();

        if condition() || task::core_current_task_kill_pending(core_id) {
            let task_ref = task::core_current_task_ref(core_id);
            queue.remove(task_ref);
            scheduler::wake_task(task_ref);
        }

        scheduler::core_run_scheduling(core_id);
    }
}

/// Blocks the running task for at least `duration_ms`, or until it is asked to exit
pub fn sleep_current_task_ms(duration_ms: u64) {
    let wake_time_us =
        crate::interrupts::tsc_timestamp_us() + duration_ms * MICROSECONDS_PER_MILLISECOND;

    let core_id = get_current_core_id();
    while crate::interrupts::tsc_timestamp_us() < wake_time_us
        && !task::core_current_task_kill_pending(core_id)
    {
        scheduler::core_block_current_task_until_wake_time(wake_time_us, core_id);
    }
}

/// Woken whenever input reaches the event buffer of a window
pub static INPUT_WAIT_QUEUE: WaitQueue = WaitQueue::new();

/// Woken by FutexWake, waiters recheck the word they sleep on
pub static FUTEX_WAIT_QUEUE: WaitQueue = WaitQueue::new();
