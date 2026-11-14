#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};
use kernel::process::elf_loader::load_elf_from_path;
use kernel::process::process_manager::{ARCHE_PID, BIGPROC_NAME, BIGPROC_PID, PROCESS_MANAGER};
use kernel::process::shared_state::{TEST_COUNTER_COUNT, get_shared_program_data_buffer_mut};
use kernel::process::task;
use kernel::process::{ChildPolicy, PID, ProcessState};
use kernel::testing::{QemuExitCode, exit_qemu, mount_test_disk, test_panic_handler};
use kernel::{boot_common, bsp_init, serial_println_core};

// indexes into the shared test counters, treeroot and treeleaf use the same values
const COUNTER_ROOT_MODE: usize = 0;
const COUNTER_ROOT_READY: usize = 1;
const COUNTER_ROOT_RELEASE: usize = 2;
const COUNTER_LEAVES_STARTED: usize = 3;
const COUNTER_LEAF_RELEASE: usize = 4;
const COUNTER_CHECKS_PASSED: usize = 5;
const COUNTER_CHECKS_FAILED: usize = 6;
const USED_COUNTER_COUNT: usize = 7;

// values of COUNTER_ROOT_MODE, treeroot uses the same values
const MODE_EXIT_KILLING_CHILDREN: u32 = 0;
const MODE_EXIT_ORPHANING_CHILDREN: u32 = 1;
const MODE_WAIT_FOR_KILL: u32 = 2;

const ROOT_PATH: &str = "/BIN/treeroot";
const ROOT_NAME: &str = "treeroot";
const ROOT_PRIORITY: u8 = 5;
const LEAF_COUNT: usize = 2;
// treeroot exits with this code, and a cascade passes it on to the leaves
const ROOT_EXIT_CODE: i32 = 3;
const KILL_EXIT_CODE: i32 = 9;
const LEAF_EXIT_CODE: i32 = 0;
// checks treeroot runs on every start
const ROOT_CHECK_COUNT: u32 = 3;
const STARTUP_DEADLINE_US: u64 = 5_000_000;
const DEADLINE_US: u64 = 10_000_000;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    unsafe { boot_common::bsp_early_init() };
    test_main();
}

fn fail(message: &str) -> ! {
    serial_println_core!("FAIL: {}", message);
    serial_println_core!("=== test_user_proctree: FAILED ===");
    exit_qemu(QemuExitCode::Failed);
}

fn counter(index: usize) -> &'static AtomicU32 {
    debug_assert!(index < TEST_COUNTER_COUNT);

    // SAFETY: the shared page lives forever and the counters are atomics
    unsafe { &get_shared_program_data_buffer_mut().test_counters[index] }
}

fn now_us() -> u64 {
    kernel::interrupts::tsc_timestamp_us()
}

fn wait_until(what: &str, deadline_us: u64, condition: impl Fn() -> bool) {
    let start_us = now_us();

    while !condition() {
        if now_us() - start_us > deadline_us {
            serial_println_core!("timed out waiting for: {}", what);
            fail("timeout");
        }
        core::hint::spin_loop();
    }
}

fn has_task(pid: PID) -> bool {
    task::find_slot_by_pid(pid).is_some()
}

fn children_of(pid: PID) -> alloc::vec::Vec<PID> {
    let process_manager = PROCESS_MANAGER.lock();
    match process_manager.get_process(pid) {
        Ok(process) => process.children.iter().copied().collect(),
        Err(_) => fail("a parent process does not exist"),
    }
}

fn check_bigproc() {
    let process_manager = PROCESS_MANAGER.lock();
    let Ok(bigproc) = process_manager.get_process(BIGPROC_PID) else {
        fail("bigproc does not exist");
    };
    if bigproc.name != BIGPROC_NAME {
        serial_println_core!("PID 1 is named {}", bigproc.name);
        fail("PID 1 is not bigproc");
    }
    if bigproc.parent_pid != ARCHE_PID || bigproc.state == ProcessState::Terminated {
        fail("bigproc is not a live child of arche");
    }
    let Ok(arche) = process_manager.get_process(ARCHE_PID) else {
        fail("arche does not exist");
    };
    if !arche.children.contains(&BIGPROC_PID) {
        fail("arche does not list bigproc as a child");
    }
    drop(process_manager);

    if !has_task(BIGPROC_PID) {
        fail("bigproc has no task");
    }
}

/// Starts treeroot under arche, waits for its leaves, checks the links, returns (root, leaves)
fn start_tree(mode: u32) -> (PID, [PID; LEAF_COUNT]) {
    for index in 0..USED_COUNTER_COUNT {
        counter(index).store(0, Ordering::Release);
    }
    counter(COUNTER_ROOT_MODE).store(mode, Ordering::Release);

    let elf_info = match load_elf_from_path(ROOT_PATH) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("load treeroot: {:?}", error);
            fail("could not load the treeroot ELF");
        }
    };
    let root_pid = match PROCESS_MANAGER.lock().create_process_from_elf(
        ARCHE_PID,
        &elf_info,
        ROOT_NAME,
        ROOT_PRIORITY,
    ) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("create_process_from_elf: {:?}", error);
            fail("could not create treeroot");
        }
    };

    wait_until("treeroot and its leaves", STARTUP_DEADLINE_US, || {
        (counter(COUNTER_ROOT_READY).load(Ordering::Acquire) == 1
            && counter(COUNTER_LEAVES_STARTED).load(Ordering::Acquire) == LEAF_COUNT as u32)
            || !has_task(root_pid)
    });
    if !has_task(root_pid) {
        fail("treeroot exited before it was ready");
    }

    let passed = counter(COUNTER_CHECKS_PASSED).load(Ordering::Acquire);
    let failed = counter(COUNTER_CHECKS_FAILED).load(Ordering::Acquire);
    if failed != 0 || passed != ROOT_CHECK_COUNT {
        serial_println_core!("treeroot checks: {} passed, {} failed", passed, failed);
        fail("treeroot policy checks failed");
    }

    if !children_of(ARCHE_PID).contains(&root_pid) {
        fail("arche does not list treeroot as a child");
    }
    let root_children = children_of(root_pid);
    if root_children.len() != LEAF_COUNT {
        serial_println_core!("treeroot children {:?}", root_children);
        fail("treeroot does not list its leaves");
    }

    let leaves = [root_children[0], root_children[1]];
    let process_manager = PROCESS_MANAGER.lock();
    for leaf_pid in leaves {
        match process_manager.get_process(leaf_pid) {
            Ok(leaf) if leaf.parent_pid == root_pid => {}
            _ => fail("a leaf is not parented to treeroot"),
        }
    }

    (root_pid, leaves)
}

fn check_terminated(pid: PID, exit_code: i32, what: &str) {
    let process_manager = PROCESS_MANAGER.lock();
    let Ok(process) = process_manager.get_process(pid) else {
        fail("a process is gone before its exit was checked");
    };
    if process.state != ProcessState::Terminated || process.exit_code != Some(exit_code) {
        serial_println_core!(
            "{} state {:?} exit code {:?}, expected {}",
            what,
            process.state,
            process.exit_code,
            exit_code
        );
        fail("a process did not terminate as expected");
    }
    if !process.children.is_empty() {
        fail("a terminated process still lists children");
    }
}

fn check_root_unlinked(root_pid: PID) {
    if children_of(ARCHE_PID).contains(&root_pid) {
        fail("arche still lists the terminated treeroot");
    }
}

/// The root is gone, its leaves must keep running under bigproc
fn check_and_release_orphans(root_pid: PID, leaves: [PID; LEAF_COUNT], root_exit_code: i32) {
    check_terminated(root_pid, root_exit_code, ROOT_NAME);
    check_root_unlinked(root_pid);

    let bigproc_children = children_of(BIGPROC_PID);
    {
        let process_manager = PROCESS_MANAGER.lock();
        for leaf_pid in leaves {
            let Ok(leaf) = process_manager.get_process(leaf_pid) else {
                fail("an orphaned leaf is gone");
            };
            if leaf.state == ProcessState::Terminated || !has_task(leaf_pid) {
                fail("an orphaned leaf was killed");
            }
            if leaf.parent_pid != BIGPROC_PID || !bigproc_children.contains(&leaf_pid) {
                serial_println_core!("leaf {} parent {}", leaf_pid, leaf.parent_pid);
                fail("bigproc did not adopt an orphaned leaf");
            }
        }
    }

    counter(COUNTER_LEAF_RELEASE).store(1, Ordering::Release);
    wait_until("the orphaned leaves to exit", DEADLINE_US, || {
        leaves.iter().all(|&leaf_pid| !has_task(leaf_pid))
    });

    for leaf_pid in leaves {
        check_terminated(leaf_pid, LEAF_EXIT_CODE, "orphaned leaf");
    }
    let bigproc_children = children_of(BIGPROC_PID);
    if leaves
        .iter()
        .any(|leaf_pid| bigproc_children.contains(leaf_pid))
    {
        fail("bigproc still lists an exited orphan");
    }
}

fn check_cascade(root_pid: PID, leaves: [PID; LEAF_COUNT], exit_code: i32) {
    wait_until("the whole tree to exit", DEADLINE_US, || {
        !has_task(root_pid) && leaves.iter().all(|&leaf_pid| !has_task(leaf_pid))
    });

    check_terminated(root_pid, exit_code, ROOT_NAME);
    check_root_unlinked(root_pid);
    for leaf_pid in leaves {
        check_terminated(leaf_pid, exit_code, "cascaded leaf");
    }
}

fn kill(pid: PID, child_policy: ChildPolicy) {
    if !task::request_kill(pid, KILL_EXIT_CODE, child_policy) {
        fail("request_kill found no task");
    }
}

fn round_exit_killing_children() {
    let (root_pid, leaves) = start_tree(MODE_EXIT_KILLING_CHILDREN);
    counter(COUNTER_ROOT_RELEASE).store(1, Ordering::Release);

    check_cascade(root_pid, leaves, ROOT_EXIT_CODE);
    serial_println_core!("exit killing children: ok");
}

fn round_exit_orphaning_children() {
    let (root_pid, leaves) = start_tree(MODE_EXIT_ORPHANING_CHILDREN);
    counter(COUNTER_ROOT_RELEASE).store(1, Ordering::Release);
    wait_until("treeroot to exit", DEADLINE_US, || !has_task(root_pid));

    check_and_release_orphans(root_pid, leaves, ROOT_EXIT_CODE);
    serial_println_core!("exit orphaning children: ok");
}

fn round_kill_killing_children() {
    let (root_pid, leaves) = start_tree(MODE_WAIT_FOR_KILL);
    kill(root_pid, ChildPolicy::KillChildren);

    check_cascade(root_pid, leaves, KILL_EXIT_CODE);
    serial_println_core!("kill killing children: ok");
}

fn round_kill_orphaning_children() {
    let (root_pid, leaves) = start_tree(MODE_WAIT_FOR_KILL);
    kill(root_pid, ChildPolicy::OrphanChildren);
    wait_until("treeroot to die", DEADLINE_US, || !has_task(root_pid));

    check_and_release_orphans(root_pid, leaves, KILL_EXIT_CODE);
    serial_println_core!("kill orphaning children: ok");
}

fn test_main() -> ! {
    serial_println_core!("=== test_user_proctree: start ===");

    mount_test_disk();
    kernel::process_start::init_userspace();
    bsp_init::wait_for_ap_cores_blocking();

    check_bigproc();

    let rounds: [fn(); 4] = [
        round_exit_killing_children,
        round_exit_orphaning_children,
        round_kill_killing_children,
        round_kill_orphaning_children,
    ];
    for round in rounds {
        round();
        PROCESS_MANAGER.lock().cleanup_dead();
    }

    // bigproc outlives every round and is still blocked, not dead
    check_bigproc();
    if !children_of(BIGPROC_PID).is_empty() {
        fail("bigproc still lists children after every orphan exited");
    }

    serial_println_core!("=== test_user_proctree: ok ===");
    exit_qemu(QemuExitCode::Success);
}
