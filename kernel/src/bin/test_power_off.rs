#![no_std]
#![no_main]

extern crate alloc;

use kernel::ipc::channel::NO_CHANNEL;
use kernel::power::{parse_s5_sleep_types, power_info};
use kernel::process::elf_loader::load_elf_from_path;
use kernel::process::process_manager::BIGPROC_PID;
use kernel::process::process_manager::{PROCESS_MANAGER, ProcessSpawn};
use kernel::process::thread;
use kernel::process::{FileDescriptor, MAX_OPEN_FILES};
use kernel::testing::{QemuExitCode, exit_qemu, mount_test_disk, test_panic_handler};
use kernel::{boot_common, bsp_init, serial_println_core};

const SHTEST_PATH: &str = "/BIN/shtest";
const SHTEST_NAME: &str = "shtest";
// shtest runs the shutdown builtin
const SHTEST_ARGS: &[u8] = b"shutdown\0";
const SHTEST_ARGC: u32 = 1;
const PROGRAM_PRIORITY: u8 = 5;
const DEADLINE_US: u64 = 10_000_000;

const AML_NAME_OP: u8 = 0x08;
const AML_ROOT_PREFIX: u8 = b'\\';
const AML_PACKAGE_OP: u8 = 0x12;
const AML_BYTE_PREFIX: u8 = 0x0A;
const AML_ZERO_OP: u8 = 0x00;
const AML_ONE_OP: u8 = 0x01;

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
    serial_println_core!("=== test_power_off: FAILED ===");
    exit_qemu(QemuExitCode::Failed);
}

fn check_s5_parser() {
    #[rustfmt::skip]
    let byte_prefixed = [
        0xAA, AML_NAME_OP, b'_', b'S', b'5', b'_', AML_PACKAGE_OP, 0x0A, 0x04,
        AML_BYTE_PREFIX, 0x05, AML_BYTE_PREFIX, 0x06, AML_ZERO_OP, AML_ZERO_OP,
    ];
    #[rustfmt::skip]
    let rooted_constants = [
        AML_NAME_OP, AML_ROOT_PREFIX, b'_', b'S', b'5', b'_', AML_PACKAGE_OP, 0x06, 0x04,
        AML_ZERO_OP, AML_ONE_OP, AML_ZERO_OP, AML_ZERO_OP,
    ];
    // bit 6 of the lead byte says one more PkgLength byte follows
    #[rustfmt::skip]
    let two_byte_length = [
        AML_NAME_OP, b'_', b'S', b'5', b'_', AML_PACKAGE_OP, 0x48, 0x00, 0x02,
        AML_BYTE_PREFIX, 0x07, AML_ONE_OP,
    ];
    let not_a_name = [
        b'_',
        b'S',
        b'5',
        b'_',
        AML_PACKAGE_OP,
        0x04,
        0x02,
        0x00,
        0x00,
    ];
    let not_a_package = [
        AML_NAME_OP,
        b'_',
        b'S',
        b'5',
        b'_',
        AML_ZERO_OP,
        0x04,
        0x02,
        0x00,
    ];

    if parse_s5_sleep_types(&byte_prefixed) != Some((5, 6)) {
        fail("_S5_ with byte prefixed values");
    }
    if parse_s5_sleep_types(&rooted_constants) != Some((0, 1)) {
        fail("_S5_ with a root prefix and constant values");
    }
    if parse_s5_sleep_types(&two_byte_length) != Some((7, 1)) {
        fail("_S5_ with a two byte PkgLength");
    }
    if parse_s5_sleep_types(&not_a_name).is_some() {
        fail("_S5_ outside a Name was accepted");
    }
    if parse_s5_sleep_types(&not_a_package).is_some() {
        fail("_S5_ without a package was accepted");
    }
    if parse_s5_sleep_types(b"no sleep object here").is_some() {
        fail("a stream without _S5_ was accepted");
    }
}

fn spawn_shtest() -> usize {
    let elf_info = match load_elf_from_path(SHTEST_PATH) {
        Ok(info) => info,
        Err(error) => {
            serial_println_core!("load shtest: {:?}", error);
            fail("could not load the shtest ELF");
        }
    };

    let spawn = ProcessSpawn {
        parent_pid: BIGPROC_PID,
        name: SHTEST_NAME,
        priority: PROGRAM_PRIORITY,
        argc: SHTEST_ARGC,
        packed_args: SHTEST_ARGS,
        file_descriptors: [FileDescriptor::CLOSED; MAX_OPEN_FILES],
        spawning_pid: BIGPROC_PID,
        stdout_channel: NO_CHANNEL,
    };
    match PROCESS_MANAGER.lock().spawn(&spawn, &elf_info) {
        Ok(pid) => pid,
        Err(error) => {
            serial_println_core!("spawn: {:?}", error);
            fail("could not create shtest");
        }
    }
}

fn test_main() -> ! {
    serial_println_core!("=== test_power_off: start ===");

    check_s5_parser();

    let info = power_info();
    serial_println_core!("test_power_off: {:?}", info);
    let Some(sleep_control) = info.sleep_control else {
        fail("no S5 information from the FADT and DSDT");
    };
    if sleep_control.pm1a_control_port == 0 {
        fail("the PM1a control port is 0");
    }

    mount_test_disk();
    kernel::process_start::init_userspace();
    bsp_init::wait_for_ap_cores_blocking();

    // xtask passes the test when QEMU exits with 0 after this line
    serial_println_core!("=== test_power_off: power request sent ===");
    let pid = spawn_shtest();

    let start_us = kernel::interrupts::tsc_timestamp_us();
    while thread::find_slot_by_pid(pid).is_some() {
        if kernel::interrupts::tsc_timestamp_us() - start_us > DEADLINE_US {
            fail("the machine is still running");
        }
        core::hint::spin_loop();
    }

    fail("shtest exited, the shutdown builtin returned");
}
