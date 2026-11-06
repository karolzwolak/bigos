#![no_std]
#![feature(alloc_error_handler)]
#![feature(portable_simd)]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use spin::Mutex;
use x86_64::structures::paging::PageTableFlags;

pub mod gfx;
pub mod terminal;

pub const SYS_CREATE_PROCESS: u64 = 0;
pub const SYS_TERMINATE_PROCESS: u64 = 1;
pub const SYS_WRITE: u64 = 2;
pub const SYS_READ: u64 = 3;
pub const SYS_GET_LINE: u64 = 4;
pub const SYS_ALLOCATE: u64 = 5;
pub const SYS_LOAD_FILE: u64 = 8;
pub const SYS_UNLOAD_FILE: u64 = 9;
pub const SYS_CREATE_WINDOW: u64 = 10;
pub const SYS_DESTROY_WINDOW: u64 = 11;
pub const SYS_MAP_WINDOW_BUFFER: u64 = 12;
pub const SYS_PRESENT_WINDOW: u64 = 13;
pub const SYS_GET_WINDOW_SIZE: u64 = 14;
pub const SYS_FOCUS_WINDOW: u64 = 15;
pub const SYS_GET_WINDOW_INFO: u64 = 16;

// Filesystem syscalls
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

pub const SYS_GET_CPU_INFO: u64 = 970;

pub const SYS_SLEEP_MS: u64 = 990;
pub const SYS_WAIT_FOR_INPUT: u64 = 991;
pub const SYS_FUTEX_WAIT: u64 = 992;
pub const SYS_FUTEX_WAKE: u64 = 993;
pub const SYS_YIELD: u64 = 998;
pub const SYS_EXIT: u64 = 999;
pub const SYS_ECHO: u64 = 997;

pub const FD_FLAG_READ: u8 = 0x01;
pub const FD_FLAG_WRITE: u8 = 0x02;

pub const FS_NAME_LEN: usize = 16;

//NOTE: using a vec to be at least somewhat dynamic with the paths
pub struct ProgramEntry {
    pub name: &'static str,
    pub elf_path: &'static str,
}
pub static KNOWN_PROGRAMS: Mutex<Vec<ProgramEntry>> = Mutex::new(Vec::new());
pub fn init_programs() {
    let mut programs = KNOWN_PROGRAMS.lock();

    if programs.is_empty() {
        programs.push(ProgramEntry {
            name: "odys",
            elf_path: "/bin/odys",
        });
    }
}

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
    pub _pad: [u8; 6],
    pub size: u64,
    pub created_time: u32,
    pub modified_time: u32,
}

impl DirEntryFlat {
    pub const fn zeroed() -> Self {
        Self {
            name: [0u8; FS_NAME_LEN],
            name_len: 0,
            is_dir: 0,
            _pad: [0u8; 6],
            size: 0,
            created_time: 0,
            modified_time: 0,
        }
    }

    pub fn name_str(&self) -> &str {
        let len = self.name_len as usize;
        core::str::from_utf8(&self.name[..len]).unwrap_or("")
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
    pub size: u64,
    pub created_time: u32,
    pub modified_time: u32,
}

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
            size: 0,
            created_time: 0,
            modified_time: 0,
        }
    }

    pub fn name_str(&self) -> &str {
        let len = self.name_len as usize;
        core::str::from_utf8(&self.name[..len]).unwrap_or("")
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

pub const INVALID_ALLOC: u64 = u64::MAX;

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

#[inline(always)]
pub unsafe fn syscall1(num: u64, a1: u64) -> u64 {
    unsafe { syscall6(num, a1, 0, 0, 0, 0, 0) }
}

#[inline(always)]
pub unsafe fn syscall2(num: u64, a1: u64, a2: u64) -> u64 {
    unsafe { syscall6(num, a1, a2, 0, 0, 0, 0) }
}

#[inline(always)]
pub unsafe fn syscall3(num: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    unsafe { syscall6(num, a1, a2, a3, 0, 0, 0) }
}

#[inline(always)]
pub unsafe fn syscall4(num: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    unsafe { syscall6(num, a1, a2, a3, a4, 0, 0) }
}

#[inline(always)]
pub unsafe fn syscall5(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    unsafe { syscall6(num, a1, a2, a3, a4, a5, 0) }
}

#[inline(always)]
pub unsafe fn sys_write(fd: u64, buf: *const u8, count: usize) -> i64 {
    unsafe { syscall3(SYS_WRITE, fd, buf as u64, count as u64) as i64 }
}

#[inline(always)]
pub unsafe fn sys_exit(code: i32) -> ! {
    unsafe { syscall1(SYS_EXIT, code as u64) };
    loop {}
}

#[inline(always)]
pub unsafe fn sys_terminate_process(pid: usize, exit_code: i32) -> bool {
    unsafe { syscall2(SYS_TERMINATE_PROCESS, pid as u64, exit_code as u64) == 0 }
}

#[inline(always)]
pub unsafe fn sys_yield() {
    unsafe { syscall1(SYS_YIELD, 0) };
}

/// Blocks the calling process for at least `duration_ms`, it uses no cpu meanwhile
#[inline(always)]
pub unsafe fn sys_sleep_ms(duration_ms: u64) {
    unsafe { syscall1(SYS_SLEEP_MS, duration_ms) };
}

/// Blocks until one of the windows of the calling process has an input event waiting
#[inline(always)]
pub unsafe fn sys_wait_for_input() {
    unsafe { syscall1(SYS_WAIT_FOR_INPUT, 0) };
}

/// Blocks while `*word == expected`, returns when a FutexWake happened and the word differs
#[inline(always)]
pub unsafe fn sys_futex_wait(word: *const u32, expected: u32) {
    unsafe { syscall2(SYS_FUTEX_WAIT, word as u64, expected as u64) };
}

#[inline(always)]
pub unsafe fn sys_futex_wake() {
    unsafe { syscall1(SYS_FUTEX_WAKE, 0) };
}

#[inline(always)]
pub unsafe fn sys_echo(val: u64) -> u64 {
    unsafe { syscall1(SYS_ECHO, val) }
}

/// Returns the base address of the allocated region, or u64::MAX on failure
#[inline(always)]
pub unsafe fn sys_allocate(size: usize) -> u64 {
    unsafe { syscall1(SYS_ALLOCATE, size as u64) }
}

#[inline(always)]
pub unsafe fn sys_create_process(elf_path: &[char], name: &[char], args_buffer: &[u8]) -> u32 {
    unsafe {
        syscall6(
            SYS_CREATE_PROCESS,
            elf_path.as_ptr() as u64,
            elf_path.len() as u64,
            name.as_ptr() as u64,
            name.len() as u64,
            args_buffer.as_ptr() as u64,
            args_buffer.len() as u64,
        ) as u32
    }
}

#[inline(always)]
pub unsafe fn sys_create_window(width: u32, height: u32, x: i32, y: i32) -> u32 {
    unsafe {
        syscall6(
            SYS_CREATE_WINDOW,
            width as u64,
            height as u64,
            x as u64,
            y as u64,
            0,
            0,
        ) as u32
    }
}

#[inline(always)]
pub unsafe fn sys_destroy_window(window_id: u32) {
    unsafe { syscall1(SYS_DESTROY_WINDOW, window_id as u64) };
}

/// Map the back buffer of a kernel window into this process's address space.
/// Returns a pointer to the first pixel (XRGB8888 u32 array), or null on failure.
#[inline(always)]
pub unsafe fn sys_map_window_buffer(window_id: u32) -> *mut u32 {
    let addr = unsafe { syscall1(SYS_MAP_WINDOW_BUFFER, window_id as u64) };
    if addr == u64::MAX {
        core::ptr::null_mut()
    } else {
        addr as *mut u32
    }
}

/// Signal the kernel that the back buffer is ready to be presented.
#[inline(always)]
pub unsafe fn sys_present_window(window_id: u32) {
    unsafe { syscall1(SYS_PRESENT_WINDOW, window_id as u64) };
}

/// Returns (width, height) of the window, or (0, 0) on failure.
#[inline(always)]
pub unsafe fn sys_get_window_size(window_id: u32) -> (u32, u32) {
    let packed = unsafe { syscall1(SYS_GET_WINDOW_SIZE, window_id as u64) };
    if packed == u64::MAX {
        (0, 0)
    } else {
        ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32)
    }
}

// --- Filesystem wrappers ---

/// Open a file or directory by path.
/// flags: FD_FLAG_READ | FD_FLAG_WRITE
/// Returns a file descriptor index, or usize::MAX on failure.
#[inline(always)]
pub unsafe fn sys_open_file(path: &str, flags: u8) -> usize {
    let ret = unsafe {
        syscall3(
            SYS_OPEN_FILE,
            path.as_ptr() as u64,
            path.len() as u64,
            flags as u64,
        )
    };
    if ret == u64::MAX {
        usize::MAX
    } else {
        ret as usize
    }
}

/// Close a file descriptor.
/// Returns true on success.
#[inline(always)]
pub unsafe fn sys_close_file(fd: usize) -> bool {
    unsafe { syscall1(SYS_CLOSE_FILE, fd as u64) != u64::MAX }
}

/// Read up to buf.len() bytes from an open fd
/// Returns the number of bytes read, or usize::MAX on failure.
#[inline(always)]
pub unsafe fn sys_read_file(fd: usize, buf: &mut [u8]) -> usize {
    let ret = unsafe {
        syscall4(
            SYS_READ_FILE,
            fd as u64,
            0,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    };
    if ret != u64::MAX {
        ret as usize
    } else {
        usize::MAX
    }
}

/// Read up to buf.len() bytes from an open fd at the current offset.
/// Returns the number of bytes read, or usize::MAX on failure.
#[inline(always)]
pub unsafe fn sys_read_file_at(fd: usize, offset: usize, buf: &mut [u8]) -> usize {
    let ret = unsafe {
        syscall4(
            SYS_READ_FILE,
            fd as u64,
            offset as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    };
    if ret != u64::MAX {
        ret as usize
    } else {
        usize::MAX
    }
}

/// Write buf to an open fd at the current offset.
/// Returns the number of bytes written, or usize::MAX on failure.
#[inline(always)]
pub unsafe fn sys_write_file(fd: usize, offset: usize, buf: &[u8]) -> usize {
    let ret = unsafe {
        syscall4(
            SYS_WRITE_FILE,
            fd as u64,
            offset as u64,
            buf.as_ptr() as u64,
            buf.len() as u64,
        )
    };
    if ret == u64::MAX {
        usize::MAX
    } else {
        ret as usize
    }
}

/// Stat a path. Fills out_stat on success. Returns true on success.
#[inline(always)]
pub unsafe fn sys_stat_file(path: &str, out_stat: &mut StatFlat) -> bool {
    let ret = unsafe {
        syscall3(
            SYS_STAT_FILE,
            path.as_ptr() as u64,
            path.len() as u64,
            out_stat as *mut StatFlat as u64,
        )
    };
    ret != u64::MAX
}

#[inline(always)]
pub unsafe fn sys_create_circular_buffer(
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
    ret != u64::MAX
}

#[inline(always)]
pub unsafe fn sys_get_window_info(window_id: u32, out_info: &mut WindowInfo) -> bool {
    let ret = unsafe {
        syscall2(
            SYS_GET_WINDOW_INFO,
            window_id as u64,
            out_info as *mut WindowInfo as u64,
        )
    };
    ret != u64::MAX
}

/// List the contents of a directory into out_entries.
/// Returns the number of entries written, or usize::MAX on failure.
#[inline(always)]
pub unsafe fn sys_list_dir(path: &str, out_entries: &mut [DirEntryFlat]) -> usize {
    let byte_len = out_entries.len() * core::mem::size_of::<DirEntryFlat>();
    let ret = unsafe {
        syscall6(
            SYS_LIST_DIR,
            path.as_ptr() as u64,
            path.len() as u64,
            out_entries.as_mut_ptr() as u64,
            byte_len as u64,
            0,
            0,
        )
    };
    if ret == u64::MAX {
        usize::MAX
    } else {
        ret as usize
    }
}

/// Create a new file at the given path. Returns true on success.
#[inline(always)]
pub unsafe fn sys_create_file(path: &str) -> bool {
    let ret = unsafe { syscall3(SYS_CREATE_FILE, path.as_ptr() as u64, path.len() as u64, 0) };
    ret != u64::MAX
}

/// Create a new directory at the given path. Returns true on success.
#[inline(always)]
pub unsafe fn sys_create_dir(path: &str) -> bool {
    let ret = unsafe { syscall3(SYS_CREATE_DIR, path.as_ptr() as u64, path.len() as u64, 0) };
    ret != u64::MAX
}

/// Delete a file or directory at the given path. Returns true on success.
#[inline(always)]
pub unsafe fn sys_delete(path: &str) -> bool {
    let ret = unsafe { syscall3(SYS_DELETE, path.as_ptr() as u64, path.len() as u64, 0) };
    ret != u64::MAX
}

#[inline(always)]
pub unsafe fn sys_get_cpu_info(out_info: &mut CpuInfoFlat) -> bool {
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
        let base = unsafe { sys_allocate(prealloc_size) };
        if base != INVALID_ALLOC {
            self.cursor
                .store(base as usize, core::sync::atomic::Ordering::Release);
            self.end.store(
                base as usize + prealloc_size,
                core::sync::atomic::Ordering::Release,
            );
        }
    }

    unsafe fn grow(&self) -> bool {
        let base = unsafe { sys_allocate(SLAB_SIZE) };
        if base != INVALID_ALLOC {
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

        if align <= 8 {
            if let Some(idx) = list_index(size) {
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
                    let base = unsafe { sys_allocate(size) };
                    if base == INVALID_ALLOC {
                        return core::ptr::null_mut();
                    }

                    return base as *mut u8;
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

        if align <= 8 {
            if let Some(idx) = list_index(size) {
                let node = ptr as *mut FreeNode;
                let mut heads = self.small_heads.lock();
                unsafe {
                    (*node).next = heads[idx];
                }
                heads[idx] = core::ptr::NonNull::new(node);
                return;
            }
        }

        debug_assert!(ptr as usize % LARGE_NODE_ALIGN == 0);
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
    unsafe { sys_exit(1) }
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
        let y_delta = (self.extra & 0xFFFF) as i16 as i16;
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
    /// The address must point to a valid, kernel-mapped EventBuffer
    pub unsafe fn new(buffer_addr: usize) -> Self {
        let buffer = unsafe { &*(buffer_addr as *const EventBuffer) };
        Self { buffer }
    }

    /// Current state, not the state at the event being processed. For that use
    /// InputEvent::key_modifiers. All keys read as up while the window is unfocused
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
            unsafe { sys_wait_for_input() }
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

pub unsafe fn tsc_read() -> (u64, u32) {
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

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = core::fmt::write(&mut $crate::Serial, core::format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = core::fmt::write(&mut $crate::Serial, core::format_args!($($arg)*));
        unsafe { $crate::sys_write(1, b"\n".as_ptr(), 1) };
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

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CacheStatsFlat {
    pub total_files: u64,
    pub total_bytes: u64,
    pub max_bytes: u64,
    pub dirty_files: u64,
}

impl CacheStatsFlat {
    pub const fn zeroed() -> Self {
        Self {
            total_files: 0,
            total_bytes: 0,
            max_bytes: 0,
            dirty_files: 0,
        }
    }
}

// Cache importance enum
#[repr(u8)]
pub enum CacheImportance {
    Minimal = 0,
    Low = 1,
    Normal = 2,
    High = 3,
    VeryHigh = 4,
    Critical = 5,
    Resident = 6,
}

// Cache syscall wrappers
pub unsafe fn sys_pin_file(path: &str) -> u64 {
    let path_bytes = path.as_bytes();
    syscall4(
        30,
        path_bytes.as_ptr() as u64,
        path_bytes.len() as u64,
        0,
        0,
    )
}

pub unsafe fn sys_unpin_file(path: &str) -> u64 {
    let path_bytes = path.as_bytes();
    syscall4(
        31,
        path_bytes.as_ptr() as u64,
        path_bytes.len() as u64,
        0,
        0,
    )
}

pub unsafe fn sys_reserve_cache(path: &str, importance: u8) -> u64 {
    let path_bytes = path.as_bytes();
    syscall4(
        32,
        path_bytes.as_ptr() as u64,
        path_bytes.len() as u64,
        importance as u64,
        0,
    )
}

pub unsafe fn sys_evict_directory(path: &str) -> u64 {
    let path_bytes = path.as_bytes();
    syscall4(
        33,
        path_bytes.as_ptr() as u64,
        path_bytes.len() as u64,
        0,
        0,
    )
}

pub unsafe fn sys_get_cache_stats(stats: &mut CacheStatsFlat) -> bool {
    let ret = syscall4(34, stats as *mut CacheStatsFlat as u64, 0, 0, 0);
    ret == 0
}

/// Flush all dirty cached files belonging to the calling process's open fds.
/// Returns true on success.
pub unsafe fn sys_flush_file_cache() -> bool {
    syscall1(35, 0) == 0
}

#[derive(Debug, PartialEq, Eq, Copy, Clone, PartialOrd, Ord)]
#[repr(u8)]
pub enum KeyCode {
    // ========= Row 1 (the F-keys) =========
    /// Top Left of the Keyboard
    Escape,
    /// Function Key F1
    F1,
    /// Function Key F2
    F2,
    /// Function Key F3
    F3,
    /// Function Key F4
    F4,
    /// Function Key F5
    F5,
    /// Function Key F6
    F6,
    /// Function Key F7
    F7,
    /// Function Key F8
    F8,
    /// Function Key F9
    F9,
    /// Function Key F10
    F10,
    /// Function Key F11
    F11,
    /// Function Key F12
    F12,

    /// The Print Screen Key
    PrintScreen,
    /// The Sys Req key (you get this keycode with Alt + PrintScreen)
    SysRq,
    /// The Scroll Lock key
    ScrollLock,
    /// The Pause/Break key
    PauseBreak,

    // ========= Row 2 (the numbers) =========
    /// Symbol key to the left of `Key1`
    Oem8,
    /// Number Line, Digit 1
    Key1,
    /// Number Line, Digit 2
    Key2,
    /// Number Line, Digit 3
    Key3,
    /// Number Line, Digit 4
    Key4,
    /// Number Line, Digit 5
    Key5,
    /// Number Line, Digit 6
    Key6,
    /// Number Line, Digit 7
    Key7,
    /// Number Line, Digit 8
    Key8,
    /// Number Line, Digit 9
    Key9,
    /// Number Line, Digit 0
    Key0,
    /// US Minus/Underscore Key (right of 'Key0')
    OemMinus,
    /// US Equals/Plus Key (right of 'OemMinus')
    OemPlus,
    /// Backspace
    Backspace,

    /// Top Left of the Extended Block
    Insert,
    /// Top Middle of the Extended Block
    Home,
    /// Top Right of the Extended Block
    PageUp,

    /// The Num Lock key
    NumpadLock,
    /// The Numpad Divide (or Slash) key
    NumpadDivide,
    /// The Numpad Multiple (or Star) key
    NumpadMultiply,
    /// The Numpad Subtract (or Minus) key
    NumpadSubtract,

    // ========= Row 3 (QWERTY) =========
    /// The Tab Key
    Tab,
    /// Letters, Top Row #1
    Q,
    /// Letters, Top Row #2
    W,
    /// Letters, Top Row #3
    E,
    /// Letters, Top Row #4
    R,
    /// Letters, Top Row #5
    T,
    /// Letters, Top Row #6
    Y,
    /// Letters, Top Row #7
    U,
    /// Letters, Top Row #8
    I,
    /// Letters, Top Row #9
    O,
    /// Letters, Top Row #10
    P,
    /// US ANSI Left-Square-Bracket key
    Oem4,
    /// US ANSI Right-Square-Bracket key
    Oem6,
    /// US ANSI Backslash Key / UK ISO Backslash Key
    Oem5,
    /// The UK/ISO Hash/Tilde key (ISO layout only)
    Oem7,

    /// The Delete key - bottom Left of the Extended Block
    Delete,
    /// The End key - bottom Middle of the Extended Block
    End,
    /// The Page Down key - -bottom Right of the Extended Block
    PageDown,

    /// The Numpad 7/Home key
    Numpad7,
    /// The Numpad 8/Up Arrow key
    Numpad8,
    /// The Numpad 9/Page Up key
    Numpad9,
    /// The Numpad Add/Plus key
    NumpadAdd,

    // ========= Row 4 (ASDF) =========
    /// Caps Lock
    CapsLock,
    /// Letters, Middle Row #1
    A,
    /// Letters, Middle Row #2
    S,
    /// Letters, Middle Row #3
    D,
    /// Letters, Middle Row #4
    F,
    /// Letters, Middle Row #5
    G,
    /// Letters, Middle Row #6
    H,
    /// Letters, Middle Row #7
    J,
    /// Letters, Middle Row #8
    K,
    /// Letters, Middle Row #9
    L,
    /// The US ANSI Semicolon/Colon key
    Oem1,
    /// The US ANSI Single-Quote/At key
    Oem3,

    /// The Return Key
    Return,

    /// The Numpad 4/Left Arrow key
    Numpad4,
    /// The Numpad 5 Key
    Numpad5,
    /// The Numpad 6/Right Arrow key
    Numpad6,

    // ========= Row 5 (ZXCV) =========
    /// Left Shift
    LShift,
    /// Letters, Bottom Row #1
    Z,
    /// Letters, Bottom Row #2
    X,
    /// Letters, Bottom Row #3
    C,
    /// Letters, Bottom Row #4
    V,
    /// Letters, Bottom Row #5
    B,
    /// Letters, Bottom Row #6
    N,
    /// Letters, Bottom Row #7
    M,
    /// US ANSI `,<` key
    OemComma,
    /// US ANSI `.>` Key
    OemPeriod,
    /// US ANSI `/?` Key
    Oem2,
    /// Right Shift
    RShift,

    /// The up-arrow in the inverted-T
    ArrowUp,

    /// Numpad 1/End Key
    Numpad1,
    /// Numpad 2/Arrow Down Key
    Numpad2,
    /// Numpad 3/Page Down Key
    Numpad3,
    /// Numpad Enter
    NumpadEnter,

    // ========= Row 6 (modifers and space bar) =========
    /// The left-hand Control key
    LControl,
    /// The left-hand 'Windows' key
    LWin,
    /// The left-hand Alt key
    LAlt,
    /// The Space Bar
    Spacebar,
    /// The right-hand AltGr key
    RAltGr,
    /// The right-hand Win key
    RWin,
    /// The 'Apps' key (aka 'Menu' or 'Right-Click')
    Apps,
    /// The right-hand Control key
    RControl,

    /// The left-arrow in the inverted-T
    ArrowLeft,
    /// The down-arrow in the inverted-T
    ArrowDown,
    /// The right-arrow in the inverted-T
    ArrowRight,

    /// The Numpad 0/Insert Key
    Numpad0,
    /// The Numppad Period/Delete Key
    NumpadPeriod,

    // ========= JIS 109-key extra keys =========
    /// Extra JIS key (0x7B)
    Oem9,
    /// Extra JIS key (0x79)
    Oem10,
    /// Extra JIS key (0x70)
    Oem11,
    /// Extra JIS symbol key (0x73)
    Oem12,
    /// Extra JIS symbol key (0x7D)
    Oem13,

    // ========= Extra Keys =========
    /// Multi-media keys - Previous Track
    PrevTrack,
    /// Multi-media keys - Next Track
    NextTrack,
    /// Multi-media keys - Volume Mute Toggle
    Mute,
    /// Multi-media keys - Open Calculator
    Calculator,
    /// Multi-media keys - Play
    Play,
    /// Multi-media keys - Stop
    Stop,
    /// Multi-media keys - Increase Volume
    VolumeDown,
    /// Multi-media keys - Decrease Volume
    VolumeUp,
    /// Multi-media keys - Open Browser
    WWWHome,
    /// Sent when the keyboard boots
    PowerOnTestOk,
    /// Sent by the keyboard when too many keys are pressed
    TooManyKeys,
    /// Used as a 'hidden' Right Control Key (Pause = RControl2 + Num Lock)
    RControl2,
    /// Used as a 'hidden' Right Alt Key (Print Screen = RAlt2 + PrntScr)
    RAlt2,
}
