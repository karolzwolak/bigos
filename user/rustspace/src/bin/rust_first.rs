#![no_std]
#![no_main]

use core::arch::global_asm;

global_asm!(
    ".section .text.entry",
    ".global _start",
    "_start:",
    "    call rust_main",
    // rust_main is -> !, so this is unreachable
    "    ud2",
);

const MSG: &[u8] = b"the userspace is in rust btw\n";

#[unsafe(no_mangle)]
pub extern "C" fn rust_main() -> ! {
    unsafe {
        let syscall_result = rustspace::sys_echo(123);
        loop {}

        // rustspace::sys_write(1, MSG.as_ptr(), MSG.len());

        // rustspace::sys_exit(0);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { rustspace::sys_exit(1) }
}
