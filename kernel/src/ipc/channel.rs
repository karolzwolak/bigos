use core::cmp::min;
use core::sync::atomic::{AtomicU32, Ordering, fence};

use spin::Mutex;
use static_assertions::const_assert;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{FrameAllocator, PageTableFlags, PhysFrame},
};

use crate::{
    memory::{
        get_frame_allocator, get_user_mem_mgr,
        paging::{MemoryMapFrameAllocator, PAGE_SIZE},
        usermem::UserMemoryManager,
    },
    process::{
        INVALID_PID, PID, PROCESS_STDOUT_CHANNEL_BASE, Process,
        process_mem::{FrameOwnership, ProcessMemoryLayout},
        syscall::SyscallError,
        thread,
        wait::{FUTEX_WAIT_QUEUE, block_current_thread_until_event},
    },
    util::cpuinfo::get_current_core_id,
};

pub type ChannelId = u16;

/// Placeholder id of a process mapping slot that holds no channel
pub const NO_CHANNEL: ChannelId = ChannelId::MAX;

const CHANNEL_SLOT_BITS: u32 = 6;
pub const MAX_CHANNELS: usize = 1 << CHANNEL_SLOT_BITS;
const CHANNEL_SLOT_MASK: ChannelId = (1 << CHANNEL_SLOT_BITS) - 1;
// above the slot bits an id counts how often its slot was reused, so a closed id never
// matches the next channel in the same slot. The count stops below the top bit, so no live
// id equals NO_CHANNEL
const CHANNEL_REUSE_BITS: u32 = ChannelId::BITS - 1 - CHANNEL_SLOT_BITS;
const CHANNEL_REUSE_MASK: ChannelId = (1 << CHANNEL_REUSE_BITS) - 1;

pub const MAX_CHANNELS_PER_PROCESS: usize = 8;
pub const CHANNEL_HEADER_SIZE_BYTES: usize = PAGE_SIZE;
pub const CHANNEL_MAX_DATA_PAGES: usize = 16;
pub const CHANNEL_MAX_DATA_SIZE_BYTES: usize = CHANNEL_MAX_DATA_PAGES * PAGE_SIZE;
/// Header plus both data views of the largest channel
pub const CHANNEL_MAX_VIRTUAL_SIZE_BYTES: u64 =
    channel_virtual_size_bytes(CHANNEL_MAX_DATA_SIZE_BYTES as u32);

const CACHE_LINE_SIZE_BYTES: usize = 64;
const WAITING: u32 = 1;
const NOT_WAITING: u32 = 0;

/// Mirrored by rustspace
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelStatus {
    Active = 0,
    WriterLeft = 1,
    ReaderLeft = 2,
}

#[repr(C, align(64))]
pub struct ChannelWriterLine {
    pub write_idx: AtomicU32,
    pub reader_wake_seq: AtomicU32,
    pub writer_waiting: AtomicU32,
    pub status: AtomicU32,
    pub data_size_bytes: AtomicU32,
}

#[repr(C, align(64))]
pub struct ChannelReaderLine {
    pub read_idx: AtomicU32,
    pub writer_wake_seq: AtomicU32,
    pub reader_waiting: AtomicU32,
}

/// Page 0 of every channel, layout shared with rustspace `ChannelHeader`
/// Both processes can write all of it, so the kernel trusts none of these values
#[repr(C)]
pub struct ChannelHeader {
    pub writer: ChannelWriterLine,
    pub reader: ChannelReaderLine,
}

const_assert!(core::mem::align_of::<ChannelWriterLine>() == CACHE_LINE_SIZE_BYTES);
const_assert!(core::mem::size_of::<ChannelHeader>() == 2 * CACHE_LINE_SIZE_BYTES);
const_assert!(core::mem::size_of::<ChannelHeader>() <= CHANNEL_HEADER_SIZE_BYTES);

/// CreateChannel result, layout shared with rustspace `ChannelInfo`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ChannelInfo {
    pub channel_id: u64,
    pub header_vaddr: u64,
    pub data_vaddr: u64,
    pub data_size_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelRole {
    Reader,
    Writer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelMapping {
    pub channel_id: ChannelId,
    pub role: ChannelRole,
    pub header_vaddr: u64,
    pub data_size_bytes: u32,
}

impl ChannelMapping {
    pub const NONE: Self = Self {
        channel_id: NO_CHANNEL,
        role: ChannelRole::Reader,
        header_vaddr: 0,
        data_size_bytes: 0,
    };

    pub const fn is_mapped(&self) -> bool {
        self.channel_id != NO_CHANNEL
    }

    pub const fn data_vaddr(&self) -> u64 {
        self.header_vaddr + CHANNEL_HEADER_SIZE_BYTES as u64
    }

    pub fn info(&self) -> ChannelInfo {
        debug_assert!(self.is_mapped());

        ChannelInfo {
            channel_id: self.channel_id as u64,
            header_vaddr: self.header_vaddr,
            data_vaddr: self.data_vaddr(),
            data_size_bytes: self.data_size_bytes as u64,
        }
    }
}

/// The header page, then the data pages mapped twice back to back so a copy never splits
const fn channel_virtual_size_bytes(data_size_bytes: u32) -> u64 {
    CHANNEL_HEADER_SIZE_BYTES as u64 + 2 * data_size_bytes as u64
}

pub const fn is_valid_data_size(data_size_bytes: usize) -> bool {
    data_size_bytes >= PAGE_SIZE
        && data_size_bytes <= CHANNEL_MAX_DATA_SIZE_BYTES
        && data_size_bytes.is_power_of_two()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterState {
    /// the reader has not handed the channel to a child yet
    NoWriterYet,
    Writing,
    /// the writer closed the channel or exited, no new writer may attach
    Finished,
}

#[derive(Debug, Clone, Copy)]
struct Channel {
    header_frame: PhysAddr,
    data_frames: [PhysAddr; CHANNEL_MAX_DATA_PAGES],
    data_page_count: usize,
    /// processes that map the channel, the frames are freed when it drops to 0
    mapper_count: u8,
    channel_id: ChannelId,
    reader_pid: PID,
    writer_state: WriterState,
}

impl Channel {
    const FREE: Self = Self {
        header_frame: PhysAddr::zero(),
        data_frames: [PhysAddr::zero(); CHANNEL_MAX_DATA_PAGES],
        data_page_count: 0,
        mapper_count: 0,
        channel_id: 0,
        reader_pid: INVALID_PID,
        writer_state: WriterState::NoWriterYet,
    };

    const fn is_free(&self) -> bool {
        self.mapper_count == 0
    }

    const fn data_size_bytes(&self) -> u32 {
        (self.data_page_count * PAGE_SIZE) as u32
    }

    fn data_frames(&self) -> &[PhysAddr] {
        &self.data_frames[..self.data_page_count]
    }

    /// The header through the HHDM, valid in any address space while the channel is live
    fn kernel_header(&self, user_memory_manager: &UserMemoryManager) -> &'static ChannelHeader {
        debug_assert!(!self.is_free());

        let header_vaddr = self.header_frame.as_u64() + user_memory_manager.phys_offset;
        // SAFETY: the frame stays allocated while mapper_count > 0 and the header is all atomics
        unsafe { &*(header_vaddr as *const ChannelHeader) }
    }

    fn free_frames(&self, frame_allocator: &mut MemoryMapFrameAllocator) {
        frame_allocator.deallocate_frame(PhysFrame::containing_address(self.header_frame));
        for frame in self.data_frames() {
            frame_allocator.deallocate_frame(PhysFrame::containing_address(*frame));
        }
    }
}

static CHANNELS: Mutex<[Channel; MAX_CHANNELS]> = Mutex::new([Channel::FREE; MAX_CHANNELS]);

/// The id the next channel in this slot gets, the previous id with its reuse count bumped
const fn next_channel_id(slot_index: usize, previous_id: ChannelId) -> ChannelId {
    debug_assert!(slot_index < MAX_CHANNELS);

    let reuse_count = ((previous_id >> CHANNEL_SLOT_BITS) + 1) & CHANNEL_REUSE_MASK;
    (reuse_count << CHANNEL_SLOT_BITS) | slot_index as ChannelId
}

/// Slot of a live channel, stale and made-up ids are rejected
fn find_live_slot(channels: &[Channel; MAX_CHANNELS], channel_id: ChannelId) -> Option<usize> {
    if channel_id == NO_CHANNEL {
        return None;
    }

    let slot_index = (channel_id & CHANNEL_SLOT_MASK) as usize;
    let channel = &channels[slot_index];
    if channel.is_free() || channel.channel_id != channel_id {
        return None;
    }

    Some(slot_index)
}

/// Live channels, for tests
pub fn live_channel_count() -> usize {
    CHANNELS
        .lock()
        .iter()
        .filter(|channel| !channel.is_free())
        .count()
}

fn free_mapping_index(process: &Process) -> Option<usize> {
    process
        .channels
        .iter()
        .position(|mapping| !mapping.is_mapped())
}

/// Maps the header and both data views at base
fn map_views(
    channel: &Channel,
    base: VirtAddr,
    role: ChannelRole,
    memory_layout: &mut ProcessMemoryLayout,
    user_memory_manager: &UserMemoryManager,
    frame_allocator: &mut MemoryMapFrameAllocator,
) -> Result<(), SyscallError> {
    let header_flags =
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
    let data_flags = match role {
        ChannelRole::Writer => header_flags,
        ChannelRole::Reader => header_flags & !PageTableFlags::WRITABLE,
    };
    let header_size = CHANNEL_HEADER_SIZE_BYTES as u64;
    let data_size = channel.data_size_bytes() as u64;
    let first_view = base + header_size;
    let second_view = first_view + data_size;

    memory_layout
        .map_frames(
            base,
            &[channel.header_frame],
            header_flags,
            FrameOwnership::Borrowed,
            user_memory_manager,
            frame_allocator,
        )
        .map_err(|_| SyscallError::OutOfMemory)?;

    if memory_layout
        .map_frames(
            first_view,
            channel.data_frames(),
            data_flags,
            FrameOwnership::Borrowed,
            user_memory_manager,
            frame_allocator,
        )
        .is_err()
    {
        memory_layout.unmap_region(base, header_size, user_memory_manager, frame_allocator);
        return Err(SyscallError::OutOfMemory);
    }

    if memory_layout
        .map_frames(
            second_view,
            channel.data_frames(),
            data_flags,
            FrameOwnership::Borrowed,
            user_memory_manager,
            frame_allocator,
        )
        .is_err()
    {
        memory_layout.unmap_region(base, header_size, user_memory_manager, frame_allocator);
        memory_layout.unmap_region(first_view, data_size, user_memory_manager, frame_allocator);
        return Err(SyscallError::OutOfMemory);
    }

    Ok(())
}

fn unmap_views(
    mapping: &ChannelMapping,
    memory_layout: &mut ProcessMemoryLayout,
    user_memory_manager: &UserMemoryManager,
    frame_allocator: &mut MemoryMapFrameAllocator,
) {
    let header_size = CHANNEL_HEADER_SIZE_BYTES as u64;
    let both_views_size = 2 * mapping.data_size_bytes as u64;

    let header_unmapped = memory_layout.unmap_region(
        VirtAddr::new(mapping.header_vaddr),
        header_size,
        user_memory_manager,
        frame_allocator,
    );
    let data_unmapped = memory_layout.unmap_region(
        VirtAddr::new(mapping.data_vaddr()),
        both_views_size,
        user_memory_manager,
        frame_allocator,
    );
    debug_assert!(header_unmapped && data_unmapped);
}

/// NOTE: The calling process becomes the reader
pub fn create_channel(
    process: &mut Process,
    data_size_bytes: usize,
) -> Result<ChannelMapping, SyscallError> {
    if !is_valid_data_size(data_size_bytes) {
        return Err(SyscallError::InvalidArgument);
    }
    let Some(mapping_index) = free_mapping_index(process) else {
        return Err(SyscallError::TooManyOpenFiles);
    };

    let user_memory_manager = get_user_mem_mgr();
    let mut channels = CHANNELS.lock();
    let Some(slot_index) = channels.iter().position(Channel::is_free) else {
        return Err(SyscallError::OutOfMemory);
    };
    let mut frame_allocator = get_frame_allocator();

    let Some(header_frame) = frame_allocator.allocate_frame() else {
        return Err(SyscallError::OutOfMemory);
    };
    user_memory_manager.zero_frame(header_frame);
    let mut channel = Channel {
        header_frame: header_frame.start_address(),
        channel_id: next_channel_id(slot_index, channels[slot_index].channel_id),
        ..Channel::FREE
    };

    let data_page_count = data_size_bytes / PAGE_SIZE;
    while channel.data_page_count < data_page_count {
        let Some(frame) = frame_allocator.allocate_frame() else {
            channel.free_frames(&mut frame_allocator);
            return Err(SyscallError::OutOfMemory);
        };
        user_memory_manager.zero_frame(frame);

        channel.data_frames[channel.data_page_count] = frame.start_address();
        channel.data_page_count += 1;
    }

    let virtual_size = channel_virtual_size_bytes(channel.data_size_bytes());
    let memory_layout = &mut process.memory_layout;
    let previous_next_alloc_vaddr = memory_layout.next_alloc_vaddr;
    let Some(base) = memory_layout.allocate_virtual_range(virtual_size as usize) else {
        channel.free_frames(&mut frame_allocator);
        return Err(SyscallError::OutOfMemory);
    };

    if let Err(error) = map_views(
        &channel,
        base,
        ChannelRole::Reader,
        memory_layout,
        user_memory_manager,
        &mut frame_allocator,
    ) {
        memory_layout.next_alloc_vaddr = previous_next_alloc_vaddr;
        channel.free_frames(&mut frame_allocator);
        return Err(error);
    }

    channel.mapper_count = 1;
    channel.reader_pid = process.pid;
    channel
        .kernel_header(user_memory_manager)
        .writer
        .data_size_bytes
        .store(channel.data_size_bytes(), Ordering::Relaxed);
    channels[slot_index] = channel;

    let mapping = ChannelMapping {
        channel_id: channel.channel_id,
        role: ChannelRole::Reader,
        header_vaddr: base.as_u64(),
        data_size_bytes: channel.data_size_bytes(),
    };
    process.channels[mapping_index] = mapping;

    Ok(mapping)
}

/// Maps the channel into a child that is not running yet, as its stdout at
/// PROCESS_STDOUT_CHANNEL_BASE. Only the reader may hand it out, and only once
pub fn attach_stdout_writer(
    child: &mut Process,
    spawning_pid: PID,
    channel_id: ChannelId,
) -> Result<(), SyscallError> {
    debug_assert!(!child.stdout_channel().is_mapped());
    let Some(mapping_index) = free_mapping_index(child) else {
        return Err(SyscallError::TooManyOpenFiles);
    };

    let user_memory_manager = get_user_mem_mgr();
    let mut channels = CHANNELS.lock();
    let Some(slot_index) = find_live_slot(&channels, channel_id) else {
        return Err(SyscallError::InvalidArgument);
    };
    let channel = &mut channels[slot_index];
    if channel.reader_pid != spawning_pid || channel.writer_state != WriterState::NoWriterYet {
        return Err(SyscallError::InvalidArgument);
    }

    let mut frame_allocator = get_frame_allocator();
    map_views(
        channel,
        VirtAddr::new(PROCESS_STDOUT_CHANNEL_BASE),
        ChannelRole::Writer,
        &mut child.memory_layout,
        user_memory_manager,
        &mut frame_allocator,
    )?;

    channel.mapper_count += 1;
    channel.writer_state = WriterState::Writing;
    child.channels[mapping_index] = ChannelMapping {
        channel_id,
        role: ChannelRole::Writer,
        header_vaddr: PROCESS_STDOUT_CHANNEL_BASE,
        data_size_bytes: channel.data_size_bytes(),
    };

    Ok(())
}

/// Drops one process reference, the views must already be unmapped
/// Returns true when the peer was told and has to be woken
fn release_locked(channels: &mut [Channel; MAX_CHANNELS], mapping: &ChannelMapping) -> bool {
    let slot_index = find_live_slot(channels, mapping.channel_id);
    debug_assert!(
        slot_index.is_some(),
        "process maps dead channel {}",
        mapping.channel_id
    );
    let Some(slot_index) = slot_index else {
        return false;
    };
    let channel = &mut channels[slot_index];
    debug_assert!(channel.mapper_count > 0);

    channel.mapper_count -= 1;
    if channel.is_free() {
        channel.free_frames(&mut get_frame_allocator());
        *channel = Channel {
            channel_id: channel.channel_id,
            ..Channel::FREE
        };
        return false;
    }

    let header = channel.kernel_header(get_user_mem_mgr());
    match mapping.role {
        ChannelRole::Writer => {
            channel.writer_state = WriterState::Finished;
            header
                .writer
                .status
                .store(ChannelStatus::WriterLeft as u32, Ordering::Release);
            header
                .writer
                .reader_wake_seq
                .fetch_add(1, Ordering::Release);
        }
        ChannelRole::Reader => {
            header
                .writer
                .status
                .store(ChannelStatus::ReaderLeft as u32, Ordering::Release);
            header
                .reader
                .writer_wake_seq
                .fetch_add(1, Ordering::Release);
        }
    }

    true
}

/// Teardown, after the address space is gone or before it was ever used
pub fn release_process_channels(process: &mut Process) {
    let mut should_wake_peers = false;
    {
        let mut channels = CHANNELS.lock();
        for mapping in process.channels.iter_mut() {
            if mapping.is_mapped() {
                should_wake_peers |= release_locked(&mut channels, mapping);
                *mapping = ChannelMapping::NONE;
            }
        }
    }

    if should_wake_peers {
        FUTEX_WAIT_QUEUE.wake_all();
    }
}

/// CloseChannel: unmaps the channel from the calling process and drops its reference
pub fn close_channel(process: &mut Process, channel_id: ChannelId) -> Result<(), SyscallError> {
    let Some(mapping_index) = process
        .channels
        .iter()
        .position(|mapping| mapping.is_mapped() && mapping.channel_id == channel_id)
    else {
        return Err(SyscallError::InvalidArgument);
    };
    let mapping = process.channels[mapping_index];

    unmap_views(
        &mapping,
        &mut process.memory_layout,
        get_user_mem_mgr(),
        &mut get_frame_allocator(),
    );
    process.channels[mapping_index] = ChannelMapping::NONE;

    let wake_peer = release_locked(&mut CHANNELS.lock(), &mapping);
    if wake_peer {
        FUTEX_WAIT_QUEUE.wake_all();
    }

    Ok(())
}

/// Copies bytes into the stdout ring of the running process through its own mapping
/// Blocks while the ring is full
/// Once the reader is gone the bytes are dropped and reported as written,
/// so a program never stalls or fails on output nobody reads
pub fn write_stdout(mapping: &ChannelMapping, bytes: &[u8]) -> Result<usize, SyscallError> {
    debug_assert!(mapping.is_mapped() && mapping.role == ChannelRole::Writer);
    debug_assert!(is_valid_data_size(mapping.data_size_bytes as usize));

    let data_size = mapping.data_size_bytes;
    let index_mask = data_size - 1;
    // SAFETY: the running process maps its channel here until it closes it, and the process
    // is inside this syscall, so the mapping cannot go away underneath
    let header = unsafe { &*(mapping.header_vaddr as *const ChannelHeader) };
    let data = mapping.data_vaddr() as *mut u8;

    let mut written_bytes = 0;
    while written_bytes < bytes.len() {
        if header.writer.status.load(Ordering::Acquire) != ChannelStatus::Active as u32 {
            return Ok(bytes.len());
        }

        let write_idx = header.writer.write_idx.load(Ordering::Relaxed);
        let read_idx = header.reader.read_idx.load(Ordering::Acquire);
        let fill = write_idx.wrapping_sub(read_idx);
        if fill > data_size {
            return Err(SyscallError::IoError);
        }

        let free = data_size - fill;
        if free == 0 {
            wait_for_reader(header, read_idx);
            if thread::core_current_thread_kill_pending(get_current_core_id()) {
                return Ok(written_bytes);
            }
            continue;
        }

        let chunk_len = min(free as usize, bytes.len() - written_bytes);
        let offset = (write_idx & index_mask) as usize;
        // SAFETY: offset + chunk_len stays inside the two data views. copy, not
        // copy_nonoverlapping: the source is user memory and may alias the ring
        unsafe {
            core::ptr::copy(
                bytes.as_ptr().add(written_bytes),
                data.add(offset),
                chunk_len,
            )
        };
        header
            .writer
            .write_idx
            .store(write_idx.wrapping_add(chunk_len as u32), Ordering::Release);
        written_bytes += chunk_len;

        wake_reader_if_waiting(header);
    }

    Ok(written_bytes)
}

fn wake_reader_if_waiting(header: &ChannelHeader) {
    fence(Ordering::SeqCst);
    if header
        .reader
        .reader_waiting
        .swap(NOT_WAITING, Ordering::AcqRel)
        != NOT_WAITING
    {
        header
            .writer
            .reader_wake_seq
            .fetch_add(1, Ordering::Release);
        FUTEX_WAIT_QUEUE.wake_all();
    }
}

fn wait_for_reader(header: &ChannelHeader, seen_read_idx: u32) {
    header
        .writer
        .writer_waiting
        .store(WAITING, Ordering::Relaxed);
    fence(Ordering::SeqCst);

    block_current_thread_until_event(&FUTEX_WAIT_QUEUE, || {
        header.reader.read_idx.load(Ordering::Acquire) != seen_read_idx
            || header.writer.status.load(Ordering::Acquire) != ChannelStatus::Active as u32
    });
}
