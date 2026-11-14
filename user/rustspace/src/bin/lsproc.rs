#![no_std]
#![no_main]

use rustspace::{ProcessInfoFlat, sys_exit, sys_list_processes};

const ENTRIES_PER_CALL: usize = 16;
const PID_COLUMN_WIDTH: usize = 5;

const EXIT_CODE_PANIC: i32 = 255;
const EXIT_CODE_LIST_FAILED: i32 = 1;

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
        "call {lsproc_main}",
        "ud2",
        lsproc_main = sym lsproc_main,
    )
}

/// Prints every process that has not terminated, one `PID NAME` line each
extern "C" fn lsproc_main() -> ! {
    let mut entries = [ProcessInfoFlat::EMPTY; ENTRIES_PER_CALL];
    let mut start_index = 0;

    rustspace::println!("{:>width$} NAME", "PID", width = PID_COLUMN_WIDTH);
    loop {
        let Ok(listed) = sys_list_processes(start_index, &mut entries) else {
            rustspace::println!("lsproc: ListProcesses failed");
            sys_exit(EXIT_CODE_LIST_FAILED)
        };

        for entry in &entries[..listed] {
            rustspace::println!(
                "{:>width$} {}",
                entry.pid,
                entry.name(),
                width = PID_COLUMN_WIDTH
            );
        }

        if listed < ENTRIES_PER_CALL {
            break;
        }
        start_index += listed;
    }

    sys_exit(0)
}
