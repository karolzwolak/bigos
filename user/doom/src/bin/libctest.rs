#![no_std]
#![no_main]

use rustspace::sys_exit;

const EXIT_CODE_PANIC: i32 = 255;

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
        "call {libctest_main}",
        "ud2",
        libctest_main = sym libctest_main,
    )
}

extern "C" fn libctest_main() -> ! {
    sys_exit(0)
}
