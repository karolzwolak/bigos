#![no_std]
#![no_main]

use core::sync::atomic::Ordering;
use rustspace::{
    BIGPROC_PID, ChildPolicy, ProcessCreateInfo, SYS_EXIT, SYS_TERMINATE_PROCESS, SyscallError,
    program_shared_data, sys_create_process, sys_exit, sys_exit_with_policy, sys_sleep_ms,
    sys_terminate_process, syscall2, syscall3,
};

// Indexes into the shared test counters, test_user_proctree and treeleaf use the same values
// They overlap fs_test and spawner, which never run in the same test kernel
const COUNTER_ROOT_MODE: usize = 0;
const COUNTER_ROOT_READY: usize = 1;
const COUNTER_ROOT_RELEASE: usize = 2;
const COUNTER_CHECKS_PASSED: usize = 5;
const COUNTER_CHECKS_FAILED: usize = 6;

// values of COUNTER_ROOT_MODE, set by the kernel test before the spawn
const MODE_EXIT_KILLING_CHILDREN: u32 = 0;
const MODE_EXIT_ORPHANING_CHILDREN: u32 = 1;
const MODE_WAIT_FOR_KILL: u32 = 2;

const LEAF_PATH: &str = "/BIN/treeleaf";
const LEAF_COUNT: usize = 2;
const LEAF_PRIORITY: u8 = 5;
const ROOT_EXIT_CODE: i32 = 3;
const INVALID_CHILD_POLICY: u64 = 7;
const POLL_INTERVAL_MS: u64 = 1;
const EXIT_CODE_PANIC: i32 = 255;
const EXIT_CODE_SPAWN_FAILED: i32 = 1;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys_exit(EXIT_CODE_PANIC)
}

// the kernel enters with a 16 byte aligned rsp, the ABI wants it 8 off at function entry
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {treeroot_main}",
        "ud2",
        treeroot_main = sym treeroot_main,
    )
}

fn check(name: &str, condition: bool) {
    let counters = &program_shared_data().test_counters;
    if condition {
        counters[COUNTER_CHECKS_PASSED].fetch_add(1, Ordering::AcqRel);
        return;
    }

    counters[COUNTER_CHECKS_FAILED].fetch_add(1, Ordering::AcqRel);
    rustspace::println!("treeroot: FAILED: {}", name);
}

fn is_error(ret: u64, expected: SyscallError) -> bool {
    ret == expected.0.wrapping_neg()
}

fn run_policy_checks(leaf_pid: u32) {
    check(
        "terminate bigproc",
        sys_terminate_process(BIGPROC_PID, ROOT_EXIT_CODE, ChildPolicy::KillChildren)
            == Err(SyscallError::PERMISSION_DENIED),
    );

    let terminate_ret = unsafe {
        syscall3(
            SYS_TERMINATE_PROCESS,
            leaf_pid as u64,
            ROOT_EXIT_CODE as u64,
            INVALID_CHILD_POLICY,
        )
    };
    check(
        "terminate with an invalid child policy",
        is_error(terminate_ret, SyscallError::INVALID_ARGUMENT),
    );

    // rejected before the exit, so the call returns
    let exit_ret = unsafe { syscall2(SYS_EXIT, ROOT_EXIT_CODE as u64, INVALID_CHILD_POLICY) };
    check(
        "exit with an invalid child policy",
        is_error(exit_ret, SyscallError::INVALID_ARGUMENT),
    );
}

/// Spawns its leaves, runs the policy checks, then ends the way COUNTER_ROOT_MODE says
extern "C" fn treeroot_main() -> ! {
    let counters = &program_shared_data().test_counters;

    let mut leaf_pids = [0u32; LEAF_COUNT];
    for leaf_pid in leaf_pids.iter_mut() {
        match sys_create_process(&ProcessCreateInfo::new(LEAF_PATH, LEAF_PRIORITY)) {
            Ok(pid) => *leaf_pid = pid,
            Err(_) => sys_exit(EXIT_CODE_SPAWN_FAILED),
        }
    }

    run_policy_checks(leaf_pids[0]);
    counters[COUNTER_ROOT_READY].store(1, Ordering::Release);

    let mode = counters[COUNTER_ROOT_MODE].load(Ordering::Acquire);
    while mode == MODE_WAIT_FOR_KILL || counters[COUNTER_ROOT_RELEASE].load(Ordering::Acquire) == 0
    {
        sys_sleep_ms(POLL_INTERVAL_MS);
    }

    match mode {
        MODE_EXIT_KILLING_CHILDREN => sys_exit(ROOT_EXIT_CODE),
        MODE_EXIT_ORPHANING_CHILDREN => {
            sys_exit_with_policy(ROOT_EXIT_CODE, ChildPolicy::OrphanChildren)
        }
        _ => sys_exit(EXIT_CODE_PANIC),
    }
}
