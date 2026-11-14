use core::cmp::min;
use core::sync::atomic::{AtomicU32, Ordering, fence};

use crate::{
    SYS_CLOSE_CHANNEL, SYS_CREATE_CHANNEL, SyscallError, sys_futex_wait, sys_futex_wake,
    syscall_ret_check, syscall1, syscall2,
};

pub const PAGE_SIZE: usize = 4096;

/// Mirrors the kernel limits of CreateChannel
pub const CHANNEL_MAX_DATA_SIZE_BYTES: usize = 16 * PAGE_SIZE;
pub const STDOUT_CHANNEL_DATA_SIZE_BYTES: usize = 16 * 1024;

const CACHE_LINE_SIZE_BYTES: usize = 64;
const WAITING: u32 = 1;
const NOT_WAITING: u32 = 0;

/// Mirrors the kernel's `ChannelStatus`
pub const CHANNEL_STATUS_ACTIVE: u32 = 0;
pub const CHANNEL_STATUS_WRITER_LEFT: u32 = 1;
pub const CHANNEL_STATUS_READER_LEFT: u32 = 2;

/// Fields the writer stores to, mirrors the kernel's `ChannelWriterLine`
#[repr(C, align(64))]
pub struct ChannelWriterLine {
    pub write_idx: AtomicU32,
    pub reader_wake_seq: AtomicU32,
    pub writer_waiting: AtomicU32,
    pub status: AtomicU32,
    pub data_size_bytes: AtomicU32,
}

/// Fields the reader stores to, mirrors the kernel's `ChannelReaderLine`
#[repr(C, align(64))]
pub struct ChannelReaderLine {
    pub read_idx: AtomicU32,
    pub writer_wake_seq: AtomicU32,
    pub reader_waiting: AtomicU32,
}

/// Layout shared with the kernel's `ChannelHeader`
#[repr(C)]
pub struct ChannelHeader {
    pub writer: ChannelWriterLine,
    pub reader: ChannelReaderLine,
}

static_assertions::const_assert!(
    core::mem::size_of::<ChannelHeader>() == 2 * CACHE_LINE_SIZE_BYTES
);

/// CreateChannel result, layout shared with the kernel's `ChannelInfo`
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ChannelInfo {
    pub channel_id: u64,
    pub header_vaddr: u64,
    pub data_vaddr: u64,
    pub data_size_bytes: u64,
}

/// Creates a channel the caller reads from.
/// Hand it to a child with `ProcessCreateInfo::with_stdout_channel`
#[inline(always)]
pub fn sys_create_channel(data_size_bytes: usize) -> Result<ChannelInfo, SyscallError> {
    let mut info = ChannelInfo::default();
    let ret = unsafe {
        syscall2(
            SYS_CREATE_CHANNEL,
            data_size_bytes as u64,
            &mut info as *mut ChannelInfo as u64,
        )
    };

    syscall_ret_check(ret).map(|_| info)
}

/// Unmaps the channel from the caller. The peer sees it closed
#[inline(always)]
pub fn sys_close_channel(channel_id: u16) -> Result<(), SyscallError> {
    syscall_ret_check(unsafe { syscall1(SYS_CLOSE_CHANNEL, channel_id as u64) }).map(|_| ())
}

/// Result of ChannelReader::check_readable
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadReadiness {
    HasBytes,
    /// the ring is empty and the writer is still there
    NothingYet,
    /// the writer is gone and every byte it wrote was read
    WriterDone,
}

pub struct ChannelReader {
    pub info: ChannelInfo,
}

impl ChannelReader {
    pub fn create(data_size_bytes: usize) -> Result<Self, SyscallError> {
        Ok(Self {
            info: sys_create_channel(data_size_bytes)?,
        })
    }

    pub fn channel_id(&self) -> u16 {
        self.info.channel_id as u16
    }

    fn header(&self) -> &ChannelHeader {
        // SAFETY: the kernel keeps the header mapped until close consumes self
        unsafe { &*(self.info.header_vaddr as *const ChannelHeader) }
    }

    pub fn ring_waiting_bytes_size(&self) -> usize {
        let header = self.header();
        let write_idx = header.writer.write_idx.load(Ordering::Acquire);
        let read_idx = header.reader.read_idx.load(Ordering::Relaxed);

        min(
            write_idx.wrapping_sub(read_idx) as usize,
            self.info.data_size_bytes as usize,
        )
    }

    pub fn has_writer_left(&self) -> bool {
        self.header().writer.status.load(Ordering::Acquire) == CHANNEL_STATUS_WRITER_LEFT
    }

    /// Copies out what is there without blocking, returns the byte count
    pub fn read_ring_available_bytes(&mut self, out: &mut [u8]) -> usize {
        let read_len = min(self.ring_waiting_bytes_size(), out.len());
        if read_len == 0 {
            return 0;
        }

        let header = self.header();
        let read_idx = header.reader.read_idx.load(Ordering::Relaxed);
        let index_mask = self.info.data_size_bytes as u32 - 1;
        let offset = (read_idx & index_mask) as usize;
        let source = (self.info.data_vaddr as usize + offset) as *const u8;

        // SAFETY: offset + read_len stays inside the two data views
        unsafe { core::ptr::copy_nonoverlapping(source, out.as_mut_ptr(), read_len) };
        header
            .reader
            .read_idx
            .store(read_idx.wrapping_add(read_len as u32), Ordering::Release);

        // pairs with the fence the writer issues between setting writer_waiting and checking
        fence(Ordering::SeqCst);
        if header
            .writer
            .writer_waiting
            .swap(NOT_WAITING, Ordering::AcqRel)
            != NOT_WAITING
        {
            header
                .reader
                .writer_wake_seq
                .fetch_add(1, Ordering::Release);
            sys_futex_wake();
        }

        read_len
    }

    /// Non-blocking wait_for_readable: reports what the reader can do right now
    pub fn check_readable(&self) -> ReadReadiness {
        // status first: bytes written before the writer left are still reported
        let writer_left = self.has_writer_left();
        if self.ring_waiting_bytes_size() > 0 {
            return ReadReadiness::HasBytes;
        }

        if writer_left {
            ReadReadiness::WriterDone
        } else {
            ReadReadiness::NothingYet
        }
    }

    /// Sleeps until bytes arrive or the writer is gone
    pub fn wait_for_readable(&self) {
        let header = self.header();
        let seen_wake_seq = header.writer.reader_wake_seq.load(Ordering::Acquire);
        header
            .reader
            .reader_waiting
            .store(WAITING, Ordering::Relaxed);
        fence(Ordering::SeqCst);

        if self.ring_waiting_bytes_size() > 0 || self.has_writer_left() {
            header
                .reader
                .reader_waiting
                .store(NOT_WAITING, Ordering::Relaxed);
            return;
        }

        let wake_seq_word = &header.writer.reader_wake_seq as *const AtomicU32 as *const u32;
        // SAFETY: the header is mapped readable for as long as self lives
        unsafe { sys_futex_wait(wake_seq_word, seen_wake_seq) };
    }

    pub fn close(self) -> Result<(), SyscallError> {
        sys_close_channel(self.channel_id())
    }
}
