#![no_std]
#![no_main]

extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    serial_print, serial_println,
    testing::{QemuExitCode, exit_qemu},
};
use limine::{
    BaseRevision,
    request::{RequestsEndMarker, RequestsStartMarker},
};

#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();
#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START: RequestsStartMarker = RequestsStartMarker::new();
#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END: RequestsEndMarker = RequestsEndMarker::new();

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    assert!(BASE_REVISION.is_supported());
    should_fail();
    serial_println!("[test did not panic]");
    exit_qemu(QemuExitCode::Failed)
}

fn should_fail() {
    serial_print!("test_should_panic::should_fail...\t");
    assert_eq!(0, 1);
}
