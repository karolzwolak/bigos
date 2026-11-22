#![no_std]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::cmp::min;
use spin::Mutex;
use x86_64::structures::paging::PageTableFlags;

pub mod channel;
pub mod gfx;
pub mod job;
pub mod shell;
pub mod terminal;

pub const SYS_CREATE_PROCESS: u64 = 0;
pub const SYS_TERMINATE_PROCESS: u64 = 1;
pub const SYS_WRITE: u64 = 2;
pub const SYS_READ: u64 = 3;
pub const SYS_GET_LINE: u64 = 4;
pub const SYS_ALLOCATE: u64 = 5;
pub const SYS_LIST_PROCESSES: u64 = 6;
pub const SYS_WAIT_PROCESS: u64 = 7;
pub const SYS_LOAD_FILE: u64 = 8;
pub const SYS_UNLOAD_FILE: u64 = 9;
pub const SYS_CREATE_WINDOW: u64 = 10;
pub const SYS_DESTROY_WINDOW: u64 = 11;
pub const SYS_MAP_WINDOW_BUFFER: u64 = 12;
pub const SYS_PRESENT_WINDOW: u64 = 13;
pub const SYS_GET_WINDOW_SIZE: u64 = 14;
pub const SYS_FOCUS_WINDOW: u64 = 15;
pub const SYS_GET_WINDOW_INFO: u64 = 16;

pub const SYS_OPEN_FILE: u64 = 20;
pub const SYS_CLOSE_FILE: u64 = 21;
pub const SYS_READ_FILE: u64 = 22;
pub const SYS_WRITE_FILE: u64 = 23;
pub const SYS_STAT_FILE: u64 = 24;
pub const SYS_LIST_DIR: u64 = 25;
pub const SYS_CREATE_FILE: u64 = 26;
pub const SYS_CREATE_DIR: u64 = 27;
pub const SYS_DELETE: u64 = 28;

pub const SYS_CREATE_CIRCULAR_BUFFER: u64 = 600;
pub const SYS_CREATE_CHANNEL: u64 = 601;
pub const SYS_CLOSE_CHANNEL: u64 = 602;

pub const SYS_GET_CPU_INFO: u64 = 970;
pub const SYS_SHUTDOWN: u64 = 980;

pub const SYS_SLEEP_MS: u64 = 990;
pub const SYS_WAIT_FOR_INPUT: u64 = 991;
pub const SYS_FUTEX_WAIT: u64 = 992;
pub const SYS_FUTEX_WAKE: u64 = 993;
pub const SYS_GET_HANDOFF_COUNT: u64 = 995;
pub const SYS_GET_PROCESS_INFO: u64 = 996;
pub const SYS_YIELD: u64 = 998;
pub const SYS_EXIT: u64 = 999;
pub const SYS_GET_PID: u64 = 997;
pub const SYS_ECHO: u64 = SYS_GET_PID;

pub const SYSCALL_NOT_IMPLEMENTED: u64 = 6u64.wrapping_neg();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyscallError(pub u64);

impl SyscallError {
    pub const INVALID_PTR: Self = Self(1);
    pub const PERMISSION_DENIED: Self = Self(2);
    pub const OUT_OF_MEMORY: Self = Self(3);
    pub const PROCESS_NOT_FOUND: Self = Self(4);
    pub const INVALID_FD: Self = Self(5);
    pub const NOT_IMPLEMENTED: Self = Self(6);
    pub const INVALID_ARGUMENT: Self = Self(7);
    pub const NOT_FOUND: Self = Self(8);
    pub const ALREADY_EXISTS: Self = Self(9);
    pub const NOT_A_DIRECTORY: Self = Self(10);
    pub const IS_A_DIRECTORY: Self = Self(11);
    pub const DIRECTORY_NOT_EMPTY: Self = Self(12);
    pub const NO_SPACE: Self = Self(13);
    pub const INVALID_PATH: Self = Self(14);
    pub const IO_ERROR: Self = Self(15);
    pub const TOO_MANY_OPEN_FILES: Self = Self(16);
    pub const INVALID_EXECUTABLE: Self = Self(17);
    pub const SYSCALL_NOT_FOUND: Self = Self(999);

    /// Short lowercase text for messages
    pub const fn description(self) -> &'static str {
        match self.0 {
            1 => "invalid pointer",
            2 => "permission denied",
            3 => "out of memory",
            4 => "no such process",
            5 => "invalid file descriptor",
            6 => "not implemented",
            7 => "invalid argument",
            8 => "not found",
            9 => "already exists",
            10 => "not a directory",
            11 => "is a directory",
            12 => "directory not empty",
            13 => "no space left",
            14 => "invalid path",
            15 => "i/o error",
            16 => "too many open files",
            17 => "not an executable",
            999 => "unknown syscall",
            _ => "unknown error",
        }
    }
}

impl core::fmt::Display for SyscallError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.description())
    }
}

// No successful return (address, id, count, packed size) sets the top bit.
fn syscall_ret_check(ret: u64) -> Result<u64, SyscallError> {
    if (ret as i64) < 0 {
        Err(SyscallError(ret.wrapping_neg()))
    } else {
        Ok(ret)
    }
}

/// Mirrors the kernel's `BIGPROC_PID`
pub const BIGPROC_PID: usize = 1;

/// Mirrors the kernel's `USER_STACK_TOP`
const USER_STACK_TOP: usize = 0x7FFF_FFFF_F000;

/// What happens to the children of a terminating process, mirrors the kernel's `ChildPolicy`
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildPolicy {
    /// every child is killed with the same exit code, and so on down the subtree
    KillChildren = 0,
    /// the children keep running and bigproc adopts them
    OrphanChildren = 1,
}

pub const FD_FLAG_READ: u8 = 0x01;
pub const FD_FLAG_WRITE: u8 = 0x02;

/// Mirrors the kernel's `MAX_OPEN_FILES`
pub const MAX_OPEN_FILES: usize = 32;

pub const FS_NAME_LEN: usize = 16;

bitflags::bitflags! {
    /// Mirrors the kernel VFS `FileAttributes`
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FileAttributes: u8 {
        const READ = 0b00000001;
        const WRITE = 0b00000010;
        const EXECUTE = 0b00000100;
        const HIDDEN = 0b00001000;
    }
}

impl FileAttributes {
    pub const DIR_DEFAULT: Self = Self::READ;
}

//NOTE: using a vec to be at least somewhat dynamic with the paths
pub struct ProgramEntry {
    pub name: &'static str,
    pub elf_path: &'static str,
}
pub static KNOWN_PROGRAMS: Mutex<Vec<ProgramEntry>> = Mutex::new(Vec::new());
pub fn lookup_program(name: &str) -> Option<&'static str> {
    let programs = KNOWN_PROGRAMS.lock();
    programs.iter().find(|p| p.name == name).map(|p| p.elf_path)
}

pub fn is_known_program(name: &str) -> bool {
    lookup_program(name).is_some()
}

pub fn register_program(name: &'static str, path: &'static str) {
    let mut programs = KNOWN_PROGRAMS.lock();

    if let Some(entry) = programs.iter_mut().find(|p| p.name == name) {
        entry.elf_path = path;
    } else {
        programs.push(ProgramEntry {
            name,
            elf_path: path,
        });
    }
}

/// Flat directory entry returned by sys_list_dir.
/// Must match the kernel-side DirEntryFlat layout exactly.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DirEntryFlat {
    pub name: [u8; FS_NAME_LEN],
    pub name_len: u8,
    pub is_dir: u8,
    /// `FileAttributes` bits
    pub attributes: u8,
    pub _pad: [u8; 5],
    pub size: u64,
    pub created_time: u32,
    pub modified_time: u32,
}

static_assertions::const_assert!(core::mem::size_of::<DirEntryFlat>() == 40);

impl DirEntryFlat {
    pub const fn zeroed() -> Self {
        Self {
            name: [0u8; FS_NAME_LEN],
            name_len: 0,
            is_dir: 0,
            attributes: 0,
            _pad: [0u8; 5],
            size: 0,
            created_time: 0,
            modified_time: 0,
        }
    }

    pub fn name_str(&self) -> &str {
        let len = self.name_len as usize;
        core::str::from_utf8(&self.name[..len]).unwrap_or("")
    }

    pub fn attributes(&self) -> FileAttributes {
        FileAttributes::from_bits_truncate(self.attributes)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CircularBufferCreateRequest {
    pub size_bytes: usize,
    pub page_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CircularBufferInfo {
    pub virtual_base: u64,
    pub view_size: u64,
    pub total_virtual_size: u64,
}

/// Flat stat result returned by sys_stat.
/// Must match the kernel-side StatFlat layout exactly.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StatFlat {
    pub name: [u8; FS_NAME_LEN],
    pub name_len: u8,
    pub is_dir: u8,
    /// `FileAttributes` bits
    pub attributes: u8,
    pub _pad: [u8; 5],
    pub size: u64,
    pub created_time: u32,
    pub modified_time: u32,
}

static_assertions::const_assert!(core::mem::size_of::<StatFlat>() == 40);

impl CircularBufferInfo {
    pub const fn zeroed() -> Self {
        Self {
            virtual_base: 0,
            view_size: 0,
            total_virtual_size: 0,
        }
    }
}

impl StatFlat {
    pub const fn zeroed() -> Self {
        Self {
            name: [0u8; FS_NAME_LEN],
            name_len: 0,
            is_dir: 0,
            attributes: 0,
            _pad: [0u8; 5],
            size: 0,
            created_time: 0,
            modified_time: 0,
        }
    }

    pub fn name_str(&self) -> &str {
        let len = self.name_len as usize;
        core::str::from_utf8(&self.name[..len]).unwrap_or("")
    }

    pub fn attributes(&self) -> FileAttributes {
        FileAttributes::from_bits_truncate(self.attributes)
    }
}

#[repr(C, align(16))]
pub struct CpuInfoFlat {
    pub vendor: u8,
    pub family: u8,
    pub model: u8,
    pub stepping: u8,
    pub display_family: u16,
    pub display_model: u8,
    pub cache_line_size: u8,
    pub apic_id: u8,
    pub features: u32,
    pub tsc_frequency_hz: u64,
    pub boot_tsc: u64,
    pub max_cpuid_leaf: u32,
    pub max_extended_cpuid_leaf: u32,
    pub core_count: u8,
}

impl CpuInfoFlat {
    pub const fn zeroed() -> Self {
        Self {
            vendor: 0,
            family: 0,
            model: 0,
            stepping: 0,
            display_family: 0,
            display_model: 0,
            cache_line_size: 0,
            apic_id: 0,
            features: 0,
            tsc_frequency_hz: 0,
            boot_tsc: 0,
            max_cpuid_leaf: 0,
            max_extended_cpuid_leaf: 0,
            core_count: 0,
        }
    }
}

/// # Safety
///
/// `num` and the arguments must form a valid call of that syscall, and every
/// pointer among them must be valid for the accesses the syscall makes.
#[inline(always)]
pub unsafe fn syscall6(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "mov r10, rcx",
            "syscall",
            inout("rax") num => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("rcx") a4,
            in("r8")  a5,
            in("r9")  a6,
            lateout("rcx") _,
            lateout("r11") _,
            lateout("r10") _,
            options(nostack),
        );
    }
    ret
}

/// # Safety
///
/// `num` and the arguments must form a valid call of that syscall, and every
/// pointer among them must be valid for the accesses the syscall makes.
#[inline(always)]
pub unsafe fn syscall1(num: u64, a1: u64) -> u64 {
    unsafe { syscall6(num, a1, 0, 0, 0, 0, 0) }
}

/// # Safety
///
/// `num` and the arguments must form a valid call of that syscall, and every
/// pointer among them must be valid for the accesses the syscall makes.
#[inline(always)]
pub unsafe fn syscall2(num: u64, a1: u64, a2: u64) -> u64 {
    unsafe { syscall6(num, a1, a2, 0, 0, 0, 0) }
}

/// # Safety
///
/// `num` and the arguments must form a valid call of that syscall, and every
/// pointer among them must be valid for the accesses the syscall makes.
#[inline(always)]
pub unsafe fn syscall3(num: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    unsafe { syscall6(num, a1, a2, a3, 0, 0, 0) }
}

/// # Safety
///
/// `num` and the arguments must form a valid call of that syscall, and every
/// pointer among them must be valid for the accesses the syscall makes.
#[inline(always)]
pub unsafe fn syscall4(num: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    unsafe { syscall6(num, a1, a2, a3, a4, 0, 0) }
}

/// # Safety
///
/// `num` and the arguments must form a valid call of that syscall, and every
/// pointer among them must be valid for the accesses the syscall makes.
#[inline(always)]
pub unsafe fn syscall5(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    unsafe { syscall6(num, a1, a2, a3, a4, a5, 0) }
}

/// # Safety
///
/// `buf` must be valid for reads of `count` bytes.
#[inline(always)]
pub unsafe fn sys_write(fd: u64, buf: *const u8, count: usize) -> i64 {
    unsafe { syscall3(SYS_WRITE, fd, buf as u64, count as u64) as i64 }
}

/// Exits and kills every child
#[inline(always)]
pub fn sys_exit(code: i32) -> ! {
    sys_exit_with_policy(code, ChildPolicy::KillChildren)
}

#[inline(always)]
pub fn sys_exit_with_policy(code: i32, child_policy: ChildPolicy) -> ! {
    unsafe { syscall2(SYS_EXIT, code as u64, child_policy as u64) };
    unreachable!("SYS_EXIT returned")
}

/// Allowed on itself and on its own children, never on bigproc
#[inline(always)]
pub fn sys_terminate_process(
    pid: usize,
    exit_code: i32,
    child_policy: ChildPolicy,
) -> Result<(), SyscallError> {
    let ret = unsafe {
        syscall3(
            SYS_TERMINATE_PROCESS,
            pid as u64,
            exit_code as u64,
            child_policy as u64,
        )
    };

    syscall_ret_check(ret).map(|_| ())
}

#[inline(always)]
pub fn sys_yield() {
    unsafe { syscall1(SYS_YIELD, 0) };
}

/// Blocks the calling process for at least `duration_ms`, it uses no cpu meanwhile
#[inline(always)]
pub fn sys_sleep_ms(duration_ms: u64) {
    unsafe { syscall1(SYS_SLEEP_MS, duration_ms) };
}

/// Blocks until one of the windows of the calling process has an input event waiting
#[inline(always)]
pub fn sys_wait_for_input() {
    unsafe { syscall1(SYS_WAIT_FOR_INPUT, 0) };
}

/// Blocks while `*word == expected`, returns when a FutexWake happened and the word differs
///
/// # Safety
///
/// `word` must be valid for reads.
#[inline(always)]
pub unsafe fn sys_futex_wait(word: *const u32, expected: u32) {
    unsafe { syscall2(SYS_FUTEX_WAIT, word as u64, expected as u64) };
}

#[inline(always)]
pub fn sys_futex_wake() {
    unsafe { syscall1(SYS_FUTEX_WAKE, 0) };
}

#[inline(always)]
pub fn sys_get_pid() -> usize {
    unsafe { syscall1(SYS_GET_PID, 0) as usize }
}

/// Returns how many runs of the calling thread began right after another thread ran on that
/// core, a growing count proves its registers and fpu state went through the save areas
#[inline(always)]
pub fn sys_get_handoff_count() -> u64 {
    unsafe { syscall1(SYS_GET_HANDOFF_COUNT, 0) }
}

#[inline(always)]
pub fn sys_echo(val: u64) -> u64 {
    unsafe { syscall1(SYS_ECHO, val) }
}

/// Returns the base address of the allocated region.
#[inline(always)]
pub fn sys_allocate(size: usize) -> Result<u64, SyscallError> {
    syscall_ret_check(unsafe { syscall1(SYS_ALLOCATE, size as u64) })
}

/// Mirrors the kernel limits of CreateProcess
pub const MAX_ARGC: usize = 64;
pub const MAX_ARGV_BYTES: usize = 4096;
pub const MAX_PROCESS_NAME_LEN: usize = 32;

const PROCESS_CREATE_RESERVED_BYTES: usize = 5;

bitflags::bitflags! {
    /// Mirrors the kernel's `ProcessCreateFlags`
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ProcessCreateFlags: u32 {
        /// the child starts with a copy of the caller's fd table, at the same fd numbers
        const INHERIT_FDS = 1 << 0;
        /// the child is parented to bigproc, so the caller's KillChildren exit leaves it alive
        const DETACHED = 1 << 1;
        /// the child gets the caller's priority, the priority field is ignored
        const INHERIT_PRIORITY = 1 << 2;
        /// stdout and stderr of the child go into stdout_channel, the caller must be its reader
        const REDIRECT_STDOUT = 1 << 3;
    }
}

/// Argument of `sys_create_process`, layout shared with the kernel's `ProcessCreateInfo`
/// The lifetime ties it to the path, name and argument buffers it points at
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessCreateInfo<'a> {
    pub path_ptr: u64,
    pub path_len: u64,
    /// name_len 0 names the process after the last path component
    pub name_ptr: u64,
    pub name_len: u64,
    /// argc NUL-terminated strings back to back, argv_size bytes in total
    pub argv_ptr: u64,
    pub argv_size: u64,
    pub argc: u32,
    pub flags: u32,
    /// ignored with INHERIT_PRIORITY
    pub priority: u8,
    /// must be zero
    pub reserved: [u8; PROCESS_CREATE_RESERVED_BYTES],
    /// read only with REDIRECT_STDOUT, must be 0 without it
    pub stdout_channel: u16,
    pub borrowed: core::marker::PhantomData<&'a [u8]>,
}

const PROCESS_CREATE_INFO_SIZE_BYTES: usize = 64;
static_assertions::const_assert!(
    core::mem::size_of::<ProcessCreateInfo>() == PROCESS_CREATE_INFO_SIZE_BYTES
);

impl<'a> ProcessCreateInfo<'a> {
    pub fn new(path: &'a str, priority: u8) -> Self {
        Self {
            path_ptr: path.as_ptr() as u64,
            path_len: path.len() as u64,
            name_ptr: 0,
            name_len: 0,
            argv_ptr: 0,
            argv_size: 0,
            argc: 0,
            flags: 0,
            priority,
            reserved: [0; PROCESS_CREATE_RESERVED_BYTES],
            stdout_channel: 0,
            borrowed: core::marker::PhantomData,
        }
    }

    pub fn with_name(mut self, name: &'a str) -> Self {
        self.name_ptr = name.as_ptr() as u64;
        self.name_len = name.len() as u64;
        self
    }

    /// `packed_args` holds `argc` NUL-terminated strings back to back, like b"ls\0-l\0"
    pub fn with_args(mut self, packed_args: &'a [u8], argc: u32) -> Self {
        self.argv_ptr = packed_args.as_ptr() as u64;
        self.argv_size = packed_args.len() as u64;
        self.argc = argc;
        self
    }

    /// Adds flags, so it combines with with_stdout_channel in any order
    pub fn with_flags(mut self, flags: ProcessCreateFlags) -> Self {
        self.flags |= flags.bits();
        self
    }

    /// The child's stdout and stderr go into the channel, the caller must be its reader
    pub fn with_stdout_channel(mut self, channel_id: u16) -> Self {
        self.flags |= ProcessCreateFlags::REDIRECT_STDOUT.bits();
        self.stdout_channel = channel_id;
        self
    }
}

/// Starts a child process, returns its pid
#[inline(always)]
pub fn sys_create_process(info: &ProcessCreateInfo) -> Result<u32, SyscallError> {
    let ret = unsafe { syscall1(SYS_CREATE_PROCESS, info as *const ProcessCreateInfo as u64) };

    syscall_ret_check(ret).map(|pid| pid as u32)
}

const PROCESS_INFO_RESERVED_BYTES: usize = 7;

/// One sys_list_processes entry, layout shared with the kernel's `ProcessInfoFlat`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessInfoFlat {
    pub pid: u64,
    pub name_len: u8,
    pub reserved: [u8; PROCESS_INFO_RESERVED_BYTES],
    /// not NUL-terminated, name_len bytes are valid
    pub name: [u8; MAX_PROCESS_NAME_LEN],
}

static_assertions::const_assert!(core::mem::size_of::<ProcessInfoFlat>() == 48);

impl ProcessInfoFlat {
    pub const EMPTY: Self = Self {
        pid: 0,
        name_len: 0,
        reserved: [0; PROCESS_INFO_RESERVED_BYTES],
        name: [0; MAX_PROCESS_NAME_LEN],
    };

    /// Names come from CreateProcess, which only accepts UTF-8
    pub fn name(&self) -> &str {
        let name_len = min(self.name_len as usize, MAX_PROCESS_NAME_LEN);
        core::str::from_utf8(&self.name[..name_len]).unwrap_or("?")
    }
}

/// Lists the processes that have not terminated, starting at entry start_index
/// Returns the number of entries written, fewer than out_entries.len() means the end
#[inline(always)]
pub fn sys_list_processes(
    start_index: usize,
    out_entries: &mut [ProcessInfoFlat],
) -> Result<usize, SyscallError> {
    let byte_len = core::mem::size_of_val(out_entries);
    let ret = unsafe {
        syscall3(
            SYS_LIST_PROCESSES,
            out_entries.as_mut_ptr() as u64,
            byte_len as u64,
            start_index as u64,
        )
    };

    syscall_ret_check(ret).map(|n| n as usize)
}

/// Blocks until the child exits, then removes it from the process list
/// returns the exit code of the child
#[inline(always)]
pub fn sys_wait_process(pid: u32) -> Result<i32, SyscallError> {
    let ret = unsafe { syscall1(SYS_WAIT_PROCESS, pid as u64) };

    syscall_ret_check(ret).map(|exit_code| exit_code as u32 as i32)
}

/// Mirrors the kernel's `ProcessRunState`
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessRunState {
    Ready = 0,
    Running = 1,
    Blocked = 2,
    Terminated = 3,
}

impl ProcessRunState {
    pub const fn from_raw(raw: u8) -> Self {
        debug_assert!(raw <= ProcessRunState::Terminated as u8);

        match raw {
            0 => ProcessRunState::Ready,
            1 => ProcessRunState::Running,
            2 => ProcessRunState::Blocked,
            _ => ProcessRunState::Terminated,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            ProcessRunState::Ready => "ready",
            ProcessRunState::Running => "running",
            ProcessRunState::Blocked => "blocked",
            ProcessRunState::Terminated => "terminated",
        }
    }
}

/// Mirrors the kernel's `PROCESS_NO_CORE_ASSIGNED`
pub const PROCESS_NO_CORE_ASSIGNED: u8 = u8::MAX;
/// Mirrors the kernel's `NO_PARENT_PID`
pub const NO_PARENT_PID: u64 = u64::MAX;

/// layout shared with the kernel's `ProcessDetailsFlat`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessDetailsFlat {
    pub pid: u64,
    pub parent_pid: u64,
    /// sum of every mapped user region, shared mappings included
    pub mapped_bytes: u64,
    /// ProcessRunState
    pub run_state: u8,
    pub priority: u8,
    pub assigned_core: u8,
    pub name_len: u8,
    pub exit_code: i32,
    pub child_count: u32,
    pub open_fd_count: u32,
    pub channel_count: u32,
    pub stdout_write_syscall_count: u32,
    pub core_migration_count: u32,
    pub handoff_count: u32,
    pub name: [u8; MAX_PROCESS_NAME_LEN],
}

static_assertions::const_assert!(core::mem::size_of::<ProcessDetailsFlat>() == 88);

impl ProcessDetailsFlat {
    pub const EMPTY: Self = Self {
        pid: 0,
        parent_pid: 0,
        mapped_bytes: 0,
        run_state: ProcessRunState::Ready as u8,
        priority: 0,
        assigned_core: PROCESS_NO_CORE_ASSIGNED,
        name_len: 0,
        exit_code: 0,
        child_count: 0,
        open_fd_count: 0,
        channel_count: 0,
        stdout_write_syscall_count: 0,
        core_migration_count: 0,
        handoff_count: 0,
        name: [0; MAX_PROCESS_NAME_LEN],
    };

    pub fn name(&self) -> &str {
        let name_len = min(self.name_len as usize, MAX_PROCESS_NAME_LEN);
        core::str::from_utf8(&self.name[..name_len]).unwrap_or("?")
    }

    pub const fn run_state(&self) -> ProcessRunState {
        ProcessRunState::from_raw(self.run_state)
    }
}

#[inline(always)]
pub fn sys_get_process_info(
    pid: usize,
    out_details: &mut ProcessDetailsFlat,
) -> Result<(), SyscallError> {
    let ret = unsafe {
        syscall3(
            SYS_GET_PROCESS_INFO,
            pid as u64,
            out_details as *mut ProcessDetailsFlat as u64,
            core::mem::size_of::<ProcessDetailsFlat>() as u64,
        )
    };

    syscall_ret_check(ret).map(|_| ())
}

#[inline(always)]
pub fn sys_shutdown() -> SyscallError {
    let ret = unsafe { syscall1(SYS_SHUTDOWN, 0) };

    match syscall_ret_check(ret) {
        Ok(_) => SyscallError::IO_ERROR,
        Err(error) => error,
    }
}

/// One argument as the kernel lays it out on the stack, the string excludes its NUL
/// Layout shared with the kernel's `ArgSlice`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ArgSlice {
    pub ptr: *const u8,
    pub len: usize,
}

/// The arguments a process was started with, valid for its whole life
#[derive(Debug, Clone, Copy)]
pub struct Args {
    slices: &'static [ArgSlice],
}

impl Args {
    /// Build from the two registers of the first entry: rdi = argc, rsi = argv
    /// The function the naked `_start` calls receives them as its first two arguments
    ///
    /// # Safety
    ///
    /// Call only with the unchanged entry values, the kernel validated the strings as UTF-8
    pub unsafe fn from_entry(argc: usize, argv: *const ArgSlice) -> Self {
        debug_assert!(argc <= MAX_ARGC);

        let slices = if argc == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(argv, argc) }
        };

        Self { slices }
    }

    pub fn len(&self) -> usize {
        self.slices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slices.is_empty()
    }

    pub fn get(&self, index: usize) -> &'static str {
        let slice = self.slices[index];

        // SAFETY: the kernel copied the string onto the stack, which lives as long as the process,
        // and checked it is UTF-8
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(slice.ptr, slice.len)) }
    }

    pub fn iter(&self) -> impl Iterator<Item = &'static str> + '_ {
        (0..self.len()).map(|index| self.get(index))
    }
}

#[inline(always)]
/// Returns the window id.
pub fn sys_create_window(width: u32, height: u32, x: i32, y: i32) -> Result<u32, SyscallError> {
    let ret = unsafe {
        syscall6(
            SYS_CREATE_WINDOW,
            width as u64,
            height as u64,
            x as u64,
            y as u64,
            0,
            0,
        )
    };
    syscall_ret_check(ret).map(|id| id as u32)
}

/// # Safety
///
/// Nothing may use the window's mapped buffers afterwards: the kernel unmaps
/// them.
#[inline(always)]
pub unsafe fn sys_destroy_window(window_id: u32) {
    unsafe { syscall1(SYS_DESTROY_WINDOW, window_id as u64) };
}

/// Map the back buffer of a kernel window into this process's address space.
/// Returns a pointer to the first pixel (XRGB8888 u32 array).
#[inline(always)]
pub fn sys_map_window_buffer(window_id: u32) -> Result<*mut u32, SyscallError> {
    syscall_ret_check(unsafe { syscall1(SYS_MAP_WINDOW_BUFFER, window_id as u64) })
        .map(|addr| addr as *mut u32)
}

/// Signal the kernel that the back buffer is ready to be presented.
#[inline(always)]
pub fn sys_present_window(window_id: u32) {
    unsafe { syscall1(SYS_PRESENT_WINDOW, window_id as u64) };
}

/// Returns (width, height) of the window.
#[inline(always)]
pub fn sys_get_window_size(window_id: u32) -> Result<(u32, u32), SyscallError> {
    syscall_ret_check(unsafe { syscall1(SYS_GET_WINDOW_SIZE, window_id as u64) })
        .map(|packed| ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32))
}

#[inline(always)]
pub fn sys_open_file(path: &str, flags: u8) -> Result<usize, SyscallError> {
    let ret = unsafe {
        syscall3(
            SYS_OPEN_FILE,
            path.as_ptr() as u64,
            path.len() as u64,
            flags as u64,
        )
    };
    syscall_ret_check(ret).map(|n| n as usize)
}

#[inline(always)]
pub fn sys_close_file(fd: usize) -> Result<(), SyscallError> {
    syscall_ret_check(unsafe { syscall1(SYS_CLOSE_FILE, fd as u64) }).map(|_| ())
}

/// Reads up to buf.len() bytes from an open fd starting at offset_bytes,
/// Returns the number of bytes read
#[inline(always)]
pub fn sys_read_file_at(
    fd: usize,
    offset_bytes: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallError> {
    let ret = unsafe {
        syscall4(
            SYS_READ_FILE,
            fd as u64,
            offset_bytes as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    };
    syscall_ret_check(ret).map(|n| n as usize)
}

/// Writes buf to an open fd starting at offset_bytes
/// Returns the number of bytes written
#[inline(always)]
pub fn sys_write_file(fd: usize, offset_bytes: usize, buf: &[u8]) -> Result<usize, SyscallError> {
    let ret = unsafe {
        syscall4(
            SYS_WRITE_FILE,
            fd as u64,
            offset_bytes as u64,
            buf.as_ptr() as u64,
            buf.len() as u64,
        )
    };
    syscall_ret_check(ret).map(|n| n as usize)
}

pub struct File {
    pub fd: usize,
    pub offset_bytes: usize,
}

impl File {
    pub fn open(path: &str, flags: u8) -> Result<Self, SyscallError> {
        let fd = sys_open_file(path, flags)?;

        Ok(Self {
            fd,
            offset_bytes: 0,
        })
    }

    /// Reads at the current position and advances it by the bytes read
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, SyscallError> {
        let read = sys_read_file_at(self.fd, self.offset_bytes, buf)?;
        self.offset_bytes += read;

        Ok(read)
    }

    /// Writes at the current position and advances it by the bytes written
    pub fn write(&mut self, buf: &[u8]) -> Result<usize, SyscallError> {
        let written = sys_write_file(self.fd, self.offset_bytes, buf)?;
        self.offset_bytes += written;

        Ok(written)
    }

    pub fn read_at(&self, offset_bytes: usize, buf: &mut [u8]) -> Result<usize, SyscallError> {
        sys_read_file_at(self.fd, offset_bytes, buf)
    }

    pub fn write_at(&self, offset_bytes: usize, buf: &[u8]) -> Result<usize, SyscallError> {
        sys_write_file(self.fd, offset_bytes, buf)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = sys_close_file(self.fd);
    }
}

#[inline(always)]
pub fn sys_stat_file(path: &str, out_stat: &mut StatFlat) -> Result<(), SyscallError> {
    let ret = unsafe {
        syscall3(
            SYS_STAT_FILE,
            path.as_ptr() as u64,
            path.len() as u64,
            out_stat as *mut StatFlat as u64,
        )
    };
    syscall_ret_check(ret).map(|_| ())
}

#[inline(always)]
pub fn sys_create_circular_buffer(
    size: usize,
    page_flags: PageTableFlags,
    out_buffer_info: &mut CircularBufferInfo,
) -> bool {
    let ret = unsafe {
        syscall3(
            SYS_CREATE_CIRCULAR_BUFFER,
            size as u64,
            page_flags.bits(),
            out_buffer_info as *mut CircularBufferInfo as u64,
        )
    };
    syscall_ret_check(ret).is_ok()
}

#[inline(always)]
pub fn sys_get_window_info(window_id: u32, out_info: &mut WindowInfo) -> bool {
    let ret = unsafe {
        syscall2(
            SYS_GET_WINDOW_INFO,
            window_id as u64,
            out_info as *mut WindowInfo as u64,
        )
    };
    syscall_ret_check(ret).is_ok()
}

/// List the contents of a directory into out_entries, starting at entry start_index.
/// Returns the number of entries written
#[inline(always)]
pub fn sys_list_dir(
    path: &str,
    start_index: usize,
    out_entries: &mut [DirEntryFlat],
) -> Result<usize, SyscallError> {
    let byte_len = core::mem::size_of_val(out_entries);
    let ret = unsafe {
        syscall6(
            SYS_LIST_DIR,
            path.as_ptr() as u64,
            path.len() as u64,
            out_entries.as_mut_ptr() as u64,
            byte_len as u64,
            start_index as u64,
            0,
        )
    };
    syscall_ret_check(ret).map(|n| n as usize)
}

#[inline(always)]
pub fn sys_create_file(path: &str) -> Result<(), SyscallError> {
    let ret = unsafe { syscall3(SYS_CREATE_FILE, path.as_ptr() as u64, path.len() as u64, 0) };
    syscall_ret_check(ret).map(|_| ())
}

#[inline(always)]
pub fn sys_create_dir(path: &str) -> Result<(), SyscallError> {
    let ret = unsafe { syscall3(SYS_CREATE_DIR, path.as_ptr() as u64, path.len() as u64, 0) };
    syscall_ret_check(ret).map(|_| ())
}

#[inline(always)]
pub fn sys_delete(path: &str) -> Result<(), SyscallError> {
    let ret = unsafe { syscall3(SYS_DELETE, path.as_ptr() as u64, path.len() as u64, 0) };
    syscall_ret_check(ret).map(|_| ())
}

#[inline(always)]
pub fn sys_get_cpu_info(out_info: &mut CpuInfoFlat) -> bool {
    let ret = unsafe {
        syscall2(
            SYS_GET_CPU_INFO,
            out_info as *mut CpuInfoFlat as u64,
            core::mem::size_of::<CpuInfoFlat>() as u64,
        )
    };
    ret == 0
}

// Arena bump allocator with size-class free lists
// Bump for fresh slabs, free lists for reuse. SLAB_SIZE 16KiB per request.
const SLAB_SIZE: usize = 16 * 1024;

const BLOCK_SIZES: &[usize] = &[8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
const NUM_SIZES: usize = 10;

struct FreeNode {
    next: Option<core::ptr::NonNull<FreeNode>>,
}

unsafe impl Send for FreeNode {}
unsafe impl Sync for FreeNode {}

struct LargeNode {
    next: Option<core::ptr::NonNull<LargeNode>>,
    size: usize,
}

unsafe impl Send for LargeNode {}
unsafe impl Sync for LargeNode {}

static_assertions::const_assert!(core::mem::size_of::<FreeNode>() <= BLOCK_SIZES[0]);

const LARGE_NODE_SIZE: usize = core::mem::size_of::<LargeNode>();
const LARGE_NODE_ALIGN: usize = core::mem::align_of::<LargeNode>();
const LARGE_REMAINDER_MIN_BYTES: usize = 16;

const fn list_index(requested: usize) -> Option<usize> {
    let mut i = 0;
    while i < NUM_SIZES {
        if BLOCK_SIZES[i] >= requested {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub struct Arena {
    cursor: core::sync::atomic::AtomicUsize,
    end: core::sync::atomic::AtomicUsize,
    small_heads: spin::Mutex<[Option<core::ptr::NonNull<FreeNode>>; NUM_SIZES]>,
    large_head: spin::Mutex<Option<core::ptr::NonNull<LargeNode>>>,
}

unsafe impl Send for Arena {}
unsafe impl Sync for Arena {}

impl Arena {
    const fn new() -> Self {
        Self {
            cursor: core::sync::atomic::AtomicUsize::new(0),
            end: core::sync::atomic::AtomicUsize::new(0),
            small_heads: spin::Mutex::new([None; NUM_SIZES]),
            large_head: spin::Mutex::new(None),
        }
    }

    pub fn preallocate(&self, prealloc_size: usize) {
        if let Ok(base) = sys_allocate(prealloc_size) {
            self.cursor
                .store(base as usize, core::sync::atomic::Ordering::Release);
            self.end.store(
                base as usize + prealloc_size,
                core::sync::atomic::Ordering::Release,
            );
        }
    }

    unsafe fn grow(&self) -> bool {
        if let Ok(base) = sys_allocate(SLAB_SIZE) {
            self.cursor
                .store(base as usize, core::sync::atomic::Ordering::Release);
            self.end.store(
                base as usize + SLAB_SIZE,
                core::sync::atomic::Ordering::Release,
            );

            return true;
        }

        false
    }
}

#[global_allocator]
pub static ALLOCATOR: Arena = Arena::new();

unsafe impl GlobalAlloc for Arena {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        use core::sync::atomic::Ordering::{AcqRel, Acquire};

        let size = layout.size();
        let align = layout.align();

        if size == 0 {
            return core::ptr::NonNull::dangling().as_ptr();
        }

        if align <= 8
            && let Some(idx) = list_index(size)
        {
            let block_size = BLOCK_SIZES[idx];
            if let Some(ptr) = {
                let mut heads = self.small_heads.lock();
                let node = heads[idx].take();
                if let Some(n) = node {
                    let next = unsafe { n.as_ref().next };
                    heads[idx] = next;
                    Some(n.as_ptr() as *mut u8)
                } else {
                    None
                }
            } {
                return ptr;
            }

            loop {
                let cursor = self.cursor.load(Acquire);
                let end = self.end.load(Acquire);
                let aligned = (cursor + block_size - 1) & !(block_size - 1);
                let next = aligned + block_size;
                if next <= end {
                    match self.cursor.compare_exchange(cursor, next, AcqRel, Acquire) {
                        Ok(_) => return aligned as *mut u8,
                        Err(_) => continue,
                    }
                }

                if !unsafe { self.grow() } {
                    return core::ptr::null_mut();
                }
            }
        }

        if size > 4096 || align > 8 {
            // the block must be able to hold a LargeNode once it is freed
            let size = size.max(LARGE_NODE_SIZE);
            let align = align.max(LARGE_NODE_ALIGN);
            let mut large = self.large_head.lock();
            let mut prev: Option<core::ptr::NonNull<LargeNode>> = None;
            let mut cur = *large;

            while let Some(node) = cur {
                let node_size = unsafe { node.as_ref().size };
                let node_ptr = node.as_ptr() as usize;
                let aligned_ptr = (node_ptr + align - 1) & !(align - 1);
                let padding = aligned_ptr - node_ptr;
                let needed = size + padding;

                if node_size >= needed {
                    let next = unsafe { node.as_ref().next };
                    match prev {
                        Some(mut p) => unsafe { p.as_mut().next = next },
                        None => *large = next,
                    }

                    let node_end = node_ptr + node_size;
                    let rem_start =
                        (aligned_ptr + size + LARGE_NODE_ALIGN - 1) & !(LARGE_NODE_ALIGN - 1);
                    if rem_start + LARGE_NODE_SIZE + LARGE_REMAINDER_MIN_BYTES <= node_end {
                        let rem_ptr = rem_start as *mut LargeNode;
                        unsafe {
                            (*rem_ptr).next = *large;
                            (*rem_ptr).size = node_end - rem_start;
                        }
                        *large = core::ptr::NonNull::new(rem_ptr);
                    }
                    core::mem::drop(large);

                    return aligned_ptr as *mut u8;
                }

                prev = cur;
                cur = unsafe { node.as_ref().next };
            }

            core::mem::drop(large);

            loop {
                let cursor = self.cursor.load(Acquire);
                let end = self.end.load(Acquire);
                let aligned = (cursor + align - 1) & !(align - 1);
                let next = aligned + size;

                if next <= end {
                    match self.cursor.compare_exchange(cursor, next, AcqRel, Acquire) {
                        Ok(_) => return aligned as *mut u8,
                        Err(_) => continue,
                    }
                }

                if size > SLAB_SIZE {
                    return sys_allocate(size)
                        .map_or(core::ptr::null_mut(), |base| base as *mut u8);
                }

                if !unsafe { self.grow() } {
                    return core::ptr::null_mut();
                }
            }
        }

        loop {
            let cursor = self.cursor.load(Acquire);
            let end = self.end.load(Acquire);
            let aligned = (cursor + align - 1) & !(align - 1);
            let next = aligned + size;

            if next <= end {
                match self.cursor.compare_exchange(cursor, next, AcqRel, Acquire) {
                    Ok(_) => return aligned as *mut u8,
                    Err(_) => continue,
                }
            }

            if !unsafe { self.grow() } {
                return core::ptr::null_mut();
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() || layout.size() == 0 {
            return;
        }
        let size = layout.size();
        let align = layout.align();

        if align <= 8
            && let Some(idx) = list_index(size)
        {
            let node = ptr as *mut FreeNode;
            let mut heads = self.small_heads.lock();
            unsafe {
                (*node).next = heads[idx];
            }
            heads[idx] = core::ptr::NonNull::new(node);
            return;
        }

        debug_assert!((ptr as usize).is_multiple_of(LARGE_NODE_ALIGN));
        let mut large = self.large_head.lock();
        let node = ptr as *mut LargeNode;
        unsafe {
            (*node).next = *large;
            (*node).size = size.max(LARGE_NODE_SIZE);
        }
        *large = core::ptr::NonNull::new(node);
    }
}

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    unsafe { sys_write(2, b"alloc error\n".as_ptr(), 12) };
    sys_exit(1)
}

use core::arch::asm;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct InputEvent {
    pub event_type: EventType,
    pub _pad: [u8; 3],
    pub value: u32,
    pub extra: u32,
    // modifiers held when a key event happened, KeyModifiers bits
    pub modifiers: u32,
}

impl InputEvent {
    pub fn key_modifiers(&self) -> KeyModifiers {
        KeyModifiers::from_bits_truncate(self.modifiers)
    }

    pub fn decode_mouse(&self) -> MouseEvent {
        let x_delta = self.value as i32 as i16;
        let y_delta = (self.extra & 0xFFFF) as i16;
        let buttons = MouseButtons::from_bits_truncate(((self.extra >> 16) as u16) & 0x07);

        let overflow_bits = (self.extra >> 19) & 0x03;
        let x_overflow = (overflow_bits & 0x01) != 0;
        let y_overflow = (overflow_bits & 0x02) != 0;

        MouseEvent {
            x_delta,
            y_delta,
            buttons,
            x_overflow,
            y_overflow,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EventType {
    None = 0,
    CharEvent = 1,
    KeyEvent = 2,
    MouseEvent = 3,
    // physical key transition, unlike KeyEvent and CharEvent it reports every key and release
    KeyStateEvent = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum KeyState {
    Pressed = 0,
    Released = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Keys {
    ArrowUp = 0x110000,
    ArrowDown = 0x110001,
    ArrowLeft = 0x110002,
    ArrowRight = 0x110003,
    LeftAlt = 0x120000,
    Backspace = 0x080000,
    Tab = 0x090000,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KeyModifiers: u32 {
        const LSHIFT = 0b0000_0001;
        const RSHIFT = 0b0000_0010;
        const LCTRL = 0b0000_0100;
        const RCTRL = 0b0000_1000;
        const LALT = 0b0001_0000;
        const RALT = 0b0010_0000;
        const NUMLOCK = 0b0100_0000;
        const CAPSLOCK = 0b1000_0000;
        const SHIFT = Self::LSHIFT.bits() | Self::RSHIFT.bits();
        const CTRL = Self::LCTRL.bits() | Self::RCTRL.bits();
        const ALT = Self::LALT.bits() | Self::RALT.bits();
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MouseButtons: u16 {
        const LEFT = 0b0000_0001;
        const RIGHT = 0b0000_0010;
        const MIDDLE = 0b0000_0100;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub x_delta: i16,
    pub y_delta: i16,
    pub buttons: MouseButtons,
    pub x_overflow: bool,
    pub y_overflow: bool,
}

pub struct AsciiChar;
impl AsciiChar {
    pub const BACKSPACE: char = '\x08';
    pub const TAB: char = '\x09';
    pub const NEWLINE: char = '\n';
    pub const CARRIAGE_RETURN: char = '\r';
    pub const ESCAPE: char = '\x1B';
    pub const DELETE: char = '\x7F';
    pub const SPACE: char = ' ';
    pub const QUESTION_MARK: char = '?';
}

const PAGE_SIZE: usize = 4096;
const KEY_COUNT: usize = 256;
const BITS_PER_WORD: usize = 64;
const KEY_BITSET_WORD_COUNT: usize = KEY_COUNT / BITS_PER_WORD;
const BUFFER_HEADER_SIZE: usize = core::mem::size_of::<AtomicU32>() * 4
    + core::mem::size_of::<AtomicU64>() * KEY_BITSET_WORD_COUNT;
const MAX_EVENT_COUNT: usize =
    (PAGE_SIZE - BUFFER_HEADER_SIZE) / core::mem::size_of::<InputEvent>();

#[repr(C, align(4096))]
pub struct EventBuffer {
    pub write_idx: AtomicU32,
    pub read_idx: AtomicU32,
    pub event_count: AtomicU32,
    // only kept current while the window is focused
    pub modifiers: AtomicU32,
    pub key_down: [AtomicU64; KEY_BITSET_WORD_COUNT],
    pub events: [InputEvent; MAX_EVENT_COUNT],
}

const _: () = assert!(core::mem::size_of::<EventBuffer>() == PAGE_SIZE);

pub struct EventReader {
    buffer: &'static EventBuffer,
}

pub const EVENT_BUFFER_ADDR: usize = 0x0000_0007_0000_0000;
pub const PROGRAM_SHARED_DATA_ADDR: usize = EVENT_BUFFER_ADDR + PAGE_SIZE;

impl EventReader {
    /// Create a new event reader for the buffer at the given address
    ///
    /// # Safety
    ///
    /// `buffer_addr` must point to a valid, kernel-mapped `EventBuffer`.
    pub unsafe fn new(buffer_addr: usize) -> Self {
        let buffer = unsafe { &*(buffer_addr as *const EventBuffer) };
        Self { buffer }
    }

    pub fn is_key_down(&self, key: KeyCode) -> bool {
        let index = key as usize;
        let word = self.buffer.key_down[index / BITS_PER_WORD].load(Ordering::Acquire);

        (word & (1u64 << (index % BITS_PER_WORD))) != 0
    }

    pub fn modifiers(&self) -> KeyModifiers {
        KeyModifiers::from_bits_truncate(self.buffer.modifiers.load(Ordering::Acquire))
    }

    pub fn has_events(&self) -> bool {
        self.buffer.event_count.load(Ordering::Acquire) > 0
    }

    pub fn try_read(&mut self, _window_id: u32) -> Option<InputEvent> {
        if self.buffer.event_count.load(Ordering::Acquire) == 0 {
            return None;
        }

        let idx = self.buffer.read_idx.load(Ordering::Acquire) as usize;
        if idx >= MAX_EVENT_COUNT {
            self.buffer.read_idx.store(0, Ordering::Release);
            return None;
        }

        //println!("read_idx: {}, window_id: {}", idx, _window_id);

        let event =
            unsafe { core::ptr::read_volatile(&self.buffer.events[idx] as *const InputEvent) };

        core::sync::atomic::fence(Ordering::Acquire);

        let new_idx = ((idx + 1) % MAX_EVENT_COUNT) as u32;
        self.buffer.read_idx.store(new_idx, Ordering::Release);

        self.buffer.event_count.fetch_sub(1, Ordering::Release);

        Some(event)
    }

    pub fn read_blocking(&mut self) -> InputEvent {
        loop {
            if let Some(event) = self.try_read(0) {
                return event;
            }
            sys_wait_for_input()
        }
    }
}

pub const TEST_COUNTER_COUNT: usize = 16;

#[repr(C, align(4096))]
pub struct ProgramSharedDataBuffer {
    pub focused_window_id: AtomicU32,
    /// Scratch counters the kernel integration tests use to coordinate user programs
    pub test_counters: [AtomicU32; TEST_COUNTER_COUNT],
}

pub fn program_shared_data() -> &'static ProgramSharedDataBuffer {
    // SAFETY: the kernel maps the shared page at this address into every process
    unsafe { &*(PROGRAM_SHARED_DATA_ADDR as *const ProgramSharedDataBuffer) }
}

pub fn tsc_read() -> (u64, u32) {
    let low: u32;
    let high: u32;
    let core: u32;

    unsafe {
        asm!(
            "lfence",
            "rdtscp",
            out("eax") low,
            out("edx") high,
            out("ecx") core,
            options(nostack, preserves_flags)
        );
    }

    let tsc = ((high as u64) << 32) | (low as u64);
    (tsc, core)
}

pub struct Serial;

impl core::fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        unsafe { sys_write(1, s.as_ptr(), s.len()) };
        Ok(())
    }
}

/// Straight into the stdout channel when the process has one, a Write syscall otherwise
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        let mut stdout = $crate::channel::StdoutWriter::begin();
        let _ = core::fmt::Write::write_fmt(&mut stdout, core::format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        let mut stdout = $crate::channel::StdoutWriter::begin();
        let _ = core::fmt::Write::write_fmt(&mut stdout, core::format_args!($($arg)*));
        stdout.write_bytes(b"\n");
    }};
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct WindowInfo {
    pub width: u32,
    pub height: u32,
    pub event_buffer_vaddr: u64,
    pub front_buffer_offset: u64,
}

impl WindowInfo {
    pub const fn zeroed() -> Self {
        Self {
            width: 0,
            height: 0,
            event_buffer_vaddr: 0,
            front_buffer_offset: 0,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone, PartialOrd, Ord)]
#[repr(u8)]
pub enum KeyCode {
    Escape,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,

    PrintScreen,
    SysRq,
    ScrollLock,
    PauseBreak,

    Oem8,
    Key1,
    Key2,
    Key3,
    Key4,
    Key5,
    Key6,
    Key7,
    Key8,
    Key9,
    Key0,
    OemMinus,
    OemPlus,
    Backspace,

    Insert,
    Home,
    PageUp,

    NumpadLock,
    NumpadDivide,
    NumpadMultiply,
    NumpadSubtract,

    Tab,
    Q,
    W,
    E,
    R,
    T,
    Y,
    U,
    I,
    O,
    P,
    Oem4,
    Oem6,
    Oem5,
    Oem7,

    Delete,
    End,
    PageDown,

    Numpad7,
    Numpad8,
    Numpad9,
    NumpadAdd,

    CapsLock,
    A,
    S,
    D,
    F,
    G,
    H,
    J,
    K,
    L,
    Oem1,
    Oem3,

    Return,

    Numpad4,
    Numpad5,
    Numpad6,

    LShift,
    Z,
    X,
    C,
    V,
    B,
    N,
    M,
    OemComma,
    OemPeriod,
    Oem2,
    RShift,

    ArrowUp,

    Numpad1,
    Numpad2,
    Numpad3,
    NumpadEnter,

    LControl,
    LWin,
    LAlt,
    Spacebar,
    RAltGr,
    RWin,
    Apps,
    RControl,

    ArrowLeft,
    ArrowDown,
    ArrowRight,

    Numpad0,
    NumpadPeriod,

    Oem9,
    Oem10,
    Oem11,
    Oem12,
    Oem13,

    PrevTrack,
    NextTrack,
    Mute,
    Calculator,
    Play,
    Stop,
    VolumeDown,
    VolumeUp,
    WWWHome,
    PowerOnTestOk,
    TooManyKeys,
    RControl2,
    RAlt2,
}
