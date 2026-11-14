#![no_std]
#![no_main]

use rustspace::{ArgSlice, sys_exit};

const EXIT_CODE_NOT_BUILT: i32 = 1;
const EXIT_CODE_PANIC: i32 = 255;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys_exit(EXIT_CODE_PANIC)
}

// the kernel enters with a 16 byte aligned rsp, the ABI wants it 8 off at function entry
// rdi and rsi still hold argc and argv when doom_main is called
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {doom_main}",
        "ud2",
        doom_main = sym doom_main,
    )
}

extern "C" fn doom_main(_argc: usize, _argv: *const ArgSlice) -> ! {
    rustspace::println!("doom: not built yet");
    sys_exit(EXIT_CODE_NOT_BUILT)
}
