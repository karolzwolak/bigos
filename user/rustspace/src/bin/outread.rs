#![no_std]
#![no_main]

use core::sync::atomic::Ordering;
use rustspace::channel::{
    CHANNEL_MAX_DATA_SIZE_BYTES, ChannelReader, PAGE_SIZE, ReadReadiness,
    STDOUT_CHANNEL_DATA_SIZE_BYTES, sys_close_channel, sys_create_channel,
};
use rustspace::{
    ProcessCreateFlags, ProcessCreateInfo, SYS_CREATE_CHANNEL, SyscallError, program_shared_data,
    sys_create_process, sys_exit, sys_yield, syscall2,
};

// Indexes into the shared test counters, test_user_stdout and outwrite use the same values
const COUNTER_CHECKS_PASSED: usize = 0;
const COUNTER_CHECKS_FAILED: usize = 1;
const COUNTER_CHECKS_DONE: usize = 2;
const COUNTER_BASIC_PID: usize = 3;
const COUNTER_FLOOD_PID: usize = 4;
const COUNTER_ORPHAN_PID: usize = 5;
const COUNTER_READER_EXITING: usize = 6;
const COUNTER_FLOOD_SYSCALL_PID: usize = 8;

const OUTWRITE_PATH: &str = "/BIN/outwrite";
const WRITER_PRIORITY: u8 = 5;
const MODE_BASIC: &[u8] = b"basic\0";
const MODE_FLOOD: &[u8] = b"flood\0";
const MODE_FLOOD_SYSCALL: &[u8] = b"floodsys\0";
const MODE_ORPHAN: &[u8] = b"orphan\0";
const MODE_ARG_COUNT: u32 = 1;

// what outwrite prints in MODE_BASIC, stdout and stderr share the channel
const EXPECTED_BASIC_OUTPUT: &[u8] = b"outwrite: hello\noutwrite: line 1\noutwrite: stderr too\n";
const BASIC_CAPTURE_BYTES: usize = 256;

// outwrite MODE_FLOOD and MODE_FLOOD_SYSCALL send this many pattern bytes
const FLOOD_TOTAL_BYTES: usize = 48 * 1024;
const FLOOD_PATTERN_PERIOD: usize = 251;
// odd and small, so reads keep landing across the ring end
const READ_CHUNK_BYTES: usize = 97;

const NON_POWER_OF_TWO_PAGES: usize = 3;
const KERNEL_SPACE_POINTER: u64 = 0xFFFF_8000_0000_1000;
// slot 63 with reuse count 511, never reached by this test
const UNKNOWN_CHANNEL_ID: u16 = 0x7FFF;
const NO_CHANNEL: u16 = u16::MAX;
const STRAY_STDOUT_CHANNEL: u16 = 5;

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
        "call {outread_main}",
        "ud2",
        outread_main = sym outread_main,
    )
}

fn check(name: &str, condition: bool) {
    let counters = &program_shared_data().test_counters;
    if condition {
        counters[COUNTER_CHECKS_PASSED].fetch_add(1, Ordering::AcqRel);
        return;
    }

    counters[COUNTER_CHECKS_FAILED].fetch_add(1, Ordering::AcqRel);
    rustspace::println!("outread: FAILED: {}", name);
}

fn writer_info(mode: &'static [u8]) -> ProcessCreateInfo<'static> {
    ProcessCreateInfo::new(OUTWRITE_PATH, WRITER_PRIORITY).with_args(mode, MODE_ARG_COUNT)
}

/// Feeds every byte to on_bytes until the writer is gone and the ring is empty
fn drain_until_closed(reader: &mut ChannelReader, mut on_bytes: impl FnMut(&[u8])) {
    let mut chunk = [0u8; READ_CHUNK_BYTES];

    loop {
        match reader.check_readable() {
            ReadReadiness::HasBytes => {
                let read_len = reader.read_ring_available_bytes(&mut chunk);
                on_bytes(&chunk[..read_len]);
                sys_yield();
            }
            ReadReadiness::NothingYet => reader.wait_for_readable(),
            ReadReadiness::WriterDone => return,
        }
    }
}

fn check_invalid_requests() {
    check(
        "create with size 0",
        sys_create_channel(0).err() == Some(SyscallError::INVALID_ARGUMENT),
    );
    check(
        "create with a size that is not a page multiple",
        sys_create_channel(PAGE_SIZE + 1).err() == Some(SyscallError::INVALID_ARGUMENT),
    );
    check(
        "create with a page count that is not a power of two",
        sys_create_channel(NON_POWER_OF_TWO_PAGES * PAGE_SIZE).err()
            == Some(SyscallError::INVALID_ARGUMENT),
    );
    check(
        "create above the max size",
        sys_create_channel(2 * CHANNEL_MAX_DATA_SIZE_BYTES).err()
            == Some(SyscallError::INVALID_ARGUMENT),
    );

    let ret = unsafe { syscall2(SYS_CREATE_CHANNEL, PAGE_SIZE as u64, KERNEL_SPACE_POINTER) };
    check(
        "create with a kernel info pointer",
        ret == SyscallError::INVALID_PTR.0.wrapping_neg(),
    );

    check(
        "close an unknown channel",
        sys_close_channel(UNKNOWN_CHANNEL_ID) == Err(SyscallError::INVALID_ARGUMENT),
    );
    check(
        "spawn into an unknown channel",
        sys_create_process(&writer_info(MODE_BASIC).with_stdout_channel(UNKNOWN_CHANNEL_ID))
            == Err(SyscallError::INVALID_ARGUMENT),
    );
    check(
        "spawn into NO_CHANNEL",
        sys_create_process(&writer_info(MODE_BASIC).with_stdout_channel(NO_CHANNEL))
            == Err(SyscallError::INVALID_ARGUMENT),
    );

    let mut stray_channel = writer_info(MODE_BASIC);
    stray_channel.stdout_channel = STRAY_STDOUT_CHANNEL;
    check(
        "spawn with a channel but no REDIRECT_STDOUT",
        sys_create_process(&stray_channel) == Err(SyscallError::INVALID_ARGUMENT),
    );
}

/// A freed slot is handed out again under a new id, the old id must stay dead
fn check_stale_id() {
    let first = sys_create_channel(PAGE_SIZE);
    check("create a channel to close", first.is_ok());
    let Ok(first) = first else {
        return;
    };
    let first_id = first.channel_id as u16;
    check("close it", sys_close_channel(first_id).is_ok());

    let second = sys_create_channel(PAGE_SIZE);
    check("create a channel after the close", second.is_ok());
    let Ok(second) = second else {
        return;
    };
    let second_id = second.channel_id as u16;

    check("the new channel gets a new id", second_id != first_id);
    check(
        "the stale id is rejected",
        sys_close_channel(first_id) == Err(SyscallError::INVALID_ARGUMENT),
    );
    check(
        "close the new channel",
        sys_close_channel(second_id).is_ok(),
    );
}

fn check_basic_output() {
    let Ok(mut reader) = ChannelReader::create(STDOUT_CHANNEL_DATA_SIZE_BYTES) else {
        check("create the basic channel", false);
        return;
    };
    let channel_id = reader.channel_id();

    let spawned = sys_create_process(&writer_info(MODE_BASIC).with_stdout_channel(channel_id));
    check("spawn the basic writer", spawned.is_ok());
    let Ok(writer_pid) = spawned else {
        return;
    };
    program_shared_data().test_counters[COUNTER_BASIC_PID].store(writer_pid, Ordering::Release);

    check(
        "spawn a second writer into the same channel",
        sys_create_process(&writer_info(MODE_BASIC).with_stdout_channel(channel_id))
            == Err(SyscallError::INVALID_ARGUMENT),
    );

    let mut captured = [0u8; BASIC_CAPTURE_BYTES];
    let mut captured_len = 0;
    drain_until_closed(&mut reader, |bytes| {
        let copy_len = bytes.len().min(BASIC_CAPTURE_BYTES - captured_len);
        captured[captured_len..captured_len + copy_len].copy_from_slice(&bytes[..copy_len]);
        captured_len += copy_len;
    });
    check(
        "basic output arrives exactly",
        &captured[..captured_len] == EXPECTED_BASIC_OUTPUT,
    );

    check("close the basic channel", reader.close().is_ok());
    check(
        "close the basic channel twice",
        sys_close_channel(channel_id) == Err(SyscallError::INVALID_ARGUMENT),
    );
}

/// A one page ring and slow reads: the writer has to block on a full ring many times
/// mode picks whether outwrite writes the ring itself or through the Write syscall
fn check_flood_backpressure(mode: &'static [u8], pid_counter: usize) {
    let Ok(mut reader) = ChannelReader::create(PAGE_SIZE) else {
        check("create the flood channel", false);
        return;
    };

    let spawned = sys_create_process(
        &writer_info(mode)
            .with_flags(ProcessCreateFlags::INHERIT_PRIORITY)
            .with_stdout_channel(reader.channel_id()),
    );
    check("spawn the flood writer", spawned.is_ok());
    let Ok(writer_pid) = spawned else {
        return;
    };
    program_shared_data().test_counters[pid_counter].store(writer_pid, Ordering::Release);

    let mut received = 0;
    let mut mismatches = 0;
    drain_until_closed(&mut reader, |bytes| {
        for byte in bytes {
            if *byte != (received % FLOOD_PATTERN_PERIOD) as u8 {
                mismatches += 1;
            }
            received += 1;
        }
    });
    check("flood bytes arrive in order", mismatches == 0);
    check("flood byte count", received == FLOOD_TOTAL_BYTES);
    check("close the flood channel", reader.close().is_ok());
}

/// The writer outlives this process, its writes after the exit must be dropped, not block
fn start_orphan_writer() {
    let Ok(reader) = ChannelReader::create(PAGE_SIZE) else {
        check("create the orphan channel", false);
        return;
    };

    let spawned = sys_create_process(
        &writer_info(MODE_ORPHAN)
            .with_flags(ProcessCreateFlags::DETACHED)
            .with_stdout_channel(reader.channel_id()),
    );
    check("spawn the orphan writer", spawned.is_ok());
    if let Ok(writer_pid) = spawned {
        program_shared_data().test_counters[COUNTER_ORPHAN_PID]
            .store(writer_pid, Ordering::Release);
    }

    // reader is never closed, the exit has to release the channel
}

extern "C" fn outread_main() -> ! {
    check_invalid_requests();
    check_stale_id();
    check_basic_output();
    check_flood_backpressure(MODE_FLOOD, COUNTER_FLOOD_PID);
    check_flood_backpressure(MODE_FLOOD_SYSCALL, COUNTER_FLOOD_SYSCALL_PID);
    start_orphan_writer();

    // never attached to a writer, the exit has to free it as well
    let unused = ChannelReader::create(STDOUT_CHANNEL_DATA_SIZE_BYTES);
    check("create a channel left open", unused.is_ok());

    let counters = &program_shared_data().test_counters;
    counters[COUNTER_CHECKS_DONE].store(1, Ordering::Release);
    counters[COUNTER_READER_EXITING].store(1, Ordering::Release);
    sys_exit(0)
}
