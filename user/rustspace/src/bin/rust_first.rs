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

#[unsafe(no_mangle)]
pub extern "C" fn rust_main() -> ! {
    rustspace::sys_echo(123);
    rustspace::sys_exit(0)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    rustspace::sys_exit(1)
}
