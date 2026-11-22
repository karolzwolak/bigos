use crate::process::scheduler;
use crate::process::thread::{self, MAX_THREADS, ThreadRef, ThreadState};
use crate::util::cpuinfo::get_current_core_id;
use crate::util::irq_mutex::IrqMutex;
use core::sync::atomic::Ordering;

const MICROSECONDS_PER_MILLISECOND: u64 = 1000;

struct WaiterList {
    waiters: [ThreadRef; MAX_THREADS],
    count: usize,
}

impl WaiterList {
    const fn new() -> Self {
        Self {
            waiters: [ThreadRef {
                slot: thread::INVALID_SLOT,
                pid: crate::process::INVALID_PID,
            }; MAX_THREADS],
            count: 0,
        }
    }

    fn is_stale(thread_ref: ThreadRef) -> bool {
        let thread = thread::slot(thread_ref.slot as usize);

        thread.pid.load(Ordering::Acquire) != thread_ref.pid
            || thread.state() != ThreadState::Blocked
    }

    fn push(&mut self, thread_ref: ThreadRef) {
        if self.count == self.waiters.len() {
            self.remove_stale();
        }
        debug_assert!(self.count < self.waiters.len());

        self.waiters[self.count] = thread_ref;
        self.count += 1;
    }

    /// Threads killed while they waited leave their entry behind
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

    fn remove(&mut self, thread_ref: ThreadRef) {
        if let Some(index) = self.waiters[..self.count]
            .iter()
            .position(|waiter| *waiter == thread_ref)
        {
            self.count -= 1;
            self.waiters[index] = self.waiters[self.count];
        }
    }
}

/// Threads that wait for something to happen, woken by whoever makes it happen
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

    pub fn add_current_thread_and_mark_blocked(&self) {
        let core_id = get_current_core_id();
        let thread_ref = thread::core_current_thread_ref(core_id);
        let thread = thread::slot(thread_ref.slot as usize);

        let mut list = self.list.lock();
        list.push(thread_ref);

        let was_running = thread.transition(ThreadState::Running, ThreadState::Blocked);
        debug_assert!(was_running);
        scheduler::core_thread_leave_run_queue(thread.assigned_core.load(Ordering::Relaxed));
    }

    pub fn remove(&self, thread_ref: ThreadRef) {
        self.list.lock().remove(thread_ref);
    }

    pub fn wake_all(&self) {
        // wake_thread takes a run queue lock, so no waiter list lock is held while it runs
        let mut woken = [ThreadRef {
            slot: thread::INVALID_SLOT,
            pid: crate::process::INVALID_PID,
        }; MAX_THREADS];
        let woken_count = {
            let mut list = self.list.lock();
            let count = list.count;
            woken[..count].copy_from_slice(&list.waiters[..count]);
            list.count = 0;
            count
        };

        for thread_ref in &woken[..woken_count] {
            scheduler::wake_thread(*thread_ref);
        }
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

pub fn block_current_thread_until_event(queue: &WaitQueue, condition: impl Fn() -> bool) {
    loop {
        // read on every pass, another core may have stolen the thread while it was blocked
        let core_id = get_current_core_id();
        if condition() || thread::core_current_thread_kill_pending(core_id) {
            return;
        }

        queue.add_current_thread_and_mark_blocked();

        if condition() || thread::core_current_thread_kill_pending(core_id) {
            let thread_ref = thread::core_current_thread_ref(core_id);
            queue.remove(thread_ref);
            scheduler::wake_thread(thread_ref);
        }

        scheduler::core_run_scheduling(core_id);
    }
}

/// Blocks the running thread for at least `duration_ms`, or until it is asked to exit
pub fn sleep_current_thread_ms(duration_ms: u64) {
    let wake_time_us =
        crate::interrupts::tsc_timestamp_us() + duration_ms * MICROSECONDS_PER_MILLISECOND;

    // the core is read on every pass, another core may have stolen the thread while it slept
    while crate::interrupts::tsc_timestamp_us() < wake_time_us
        && !thread::core_current_thread_kill_pending(get_current_core_id())
    {
        scheduler::core_block_current_thread_until_wake_time(wake_time_us, get_current_core_id());
    }
}

/// Woken whenever input reaches the event buffer of a window
pub static INPUT_WAIT_QUEUE: WaitQueue = WaitQueue::new();

/// Woken after the compositor swapped window buffers, PresentWindow callers recheck their window
pub static WINDOW_SWAP_WAIT_QUEUE: WaitQueue = WaitQueue::new();

/// Woken by FutexWake, waiters recheck the word they sleep on
pub static FUTEX_WAIT_QUEUE: WaitQueue = WaitQueue::new();

/// Woken whenever a process terminated, WaitProcess callers recheck their child
pub static PROCESS_EXIT_WAIT_QUEUE: WaitQueue = WaitQueue::new();
