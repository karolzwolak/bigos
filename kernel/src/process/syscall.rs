use crate::data_structures::circular_buffer::{CircularBuffer, CircularBufferInfo};
use crate::events::event_buffer::EventBuffer;
use crate::graphics::FRAMEBUFFER_BYTES_PER_PIXEL;
use crate::graphics::window::{WindowInfo, event_buffer_user_vaddr};
use crate::interrupts::{BOOT_TSC, TSC_FREQUENCY_HZ};
use crate::io::serial;
use crate::memory::get_frame_allocator;
use crate::memory::memory::PAGE_SIZE;
use crate::memory::usermem::USER_MEM_MAX_ADDRESS;
use crate::process::core_pool::TOTAL_CORE_COUNT;
use crate::process::process::FileDescriptor;
use crate::process::process_mem::FrameOwnership;
use crate::process::CORE_POOL;
use crate::serial_println;
use crate::util::cpuinfo::{CpuInfoFlat, get_cpu_info_for_core};
use crate::util::msr::msr_write;
use crate::{
    process::{
        process::INVALID_PID,
        process_manager::{ARCHE_PID, PROCESS_MANAGER},
        scheduler,
    },
    serial_println_core,
    util::cpuinfo::get_current_core_id,
};
use core::arch::naked_asm;
use core::sync::atomic::Ordering;
use x86_64::registers::model_specific::{Efer, EferFlags};
use x86_64::structures::paging::{Mapper, PageTableFlags};

const SYSCALL_DEBUG_PRINT: bool = false;

#[inline]
fn validate_user_ptr(ptr: usize, len: usize) -> bool {
    let end = ptr.saturating_add(len);
    ptr != 0 && end <= USER_MEM_MAX_ADDRESS && end >= ptr
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallError {
    Success = 0,
    InvalidPtr = 1,
    PermissionDenied = 2,
    OutOfMemory = 3,
    ProcessNotFound = 4,
    InvalidFd = 5,
    NotImplemented = 6,
    SyscallNotFound = 999,
}

impl SyscallError {
    /// Negated so an error can't be mistaken for a valid fd or byte count.
    pub const fn as_return(self) -> u64 {
        (self as u64).wrapping_neg()
    }
}

#[repr(u64)]
#[derive(Debug)]
pub enum SyscallNumber {
    CreateProcess = 0,
    TerminateProcess = 1,
    Write = 2,
    Read = 3,
    GetLine = 4,
    Allocate = 5,
    LoadFile = 8,
    UnloadFile = 9,
    CreateWindow = 10,
    DestroyWindow = 11,
    MapWindowBuffer = 12,
    PresentWindow = 13,
    GetWindowSize = 14,
    FocusWindow = 15,
    GetWindowInfo = 16,
    // Filesystem syscalls
    OpenFile = 20,
    CloseFile = 21,
    ReadFile = 22,
    WriteFile = 23,
    StatFile = 24,
    ListDir = 25,
    CreateFile = 26,
    CreateDir = 27,
    Delete = 28,
    PinFile = 30,
    UnpinFile = 31,
    ReserveCache = 32,
    EvictDirectory = 33,
    GetCacheStats = 34,
    FlushFileCache = 35,

    CreateCircularBuffer = 600,

    SleepMs = 990,
    WaitForInput = 991,
    FutexWait = 992,
    FutexWake = 993,

    GetCpuInfo = 970,
    GetProcessInfo = 996,
    GetPID = 997,
    Yield = 998,
    Exit = 999,
}

/// Reached through GS after swapgs, the offsets are used by syscall_handler
#[repr(C, align(64))]
struct PerCoreSyscallData {
    /// Kernel stack top of the task running on this core, set on every dispatch
    stack_top: u64,
    user_rsp_scratch: u64,
}

impl PerCoreSyscallData {
    const fn zeroed() -> Self {
        Self {
            stack_top: 0,
            user_rsp_scratch: 0,
        }
    }
}

const SYSCALL_USER_RSP_SCRATCH_OFFSET: usize =
    core::mem::offset_of!(PerCoreSyscallData, user_rsp_scratch);

static mut SYSCALL_DATA: [PerCoreSyscallData; crate::MAX_CORES as usize] =
    [const { PerCoreSyscallData::zeroed() }; crate::MAX_CORES as usize];

pub fn set_syscall_stack_top(core_id: u8, stack_top: u64) {
    // SAFETY: only the owning core writes its entry, with interrupts off
    unsafe { SYSCALL_DATA[core_id as usize].stack_top = stack_top };
}

const MSR_KERNEL_GS_BASE: u32 = 0xC0000102;

#[repr(C)]
pub struct SyscallFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,

    pub arg6: u64,        // r9
    pub arg5: u64,        // r8
    pub arg4: u64,        // r10
    pub arg3: u64,        // rdx
    pub arg2: u64,        // rsi
    pub arg1: u64,        // rdi
    pub syscall_num: u64, // rax

    pub rflags: u64,   // r11
    pub user_rip: u64, // rcx
    pub user_rsp: u64,
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn syscall_handler() -> ! {
    naked_asm!(
        "swapgs",
        "mov gs:[{user_rsp_offset}], rsp",
        "mov rsp, gs:0",
        "push qword ptr gs:[{user_rsp_offset}]",
        "swapgs",

        "push rcx",
        "push r11",

        "push rax",
        "push rdi",
        "push rsi",
        "push rdx",
        "push r10",
        "push r8",
        "push r9",
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        "mov rdi, rsp",

        // the task stack top is 16 aligned and 16 regs were pushed, so rsp is aligned for the call
        "call {handle_syscall}",

        // RSP will point at the user stack while still in ring 0, sysretq restores IF from r11
        "cli",

        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",
        "pop r9",
        "pop r8",
        "pop r10",
        "pop rdx",
        "pop rsi",
        "pop rdi",
        "add rsp, 8",

        "pop r11",
        "pop rcx",
        "pop rsp",

        "sysretq",

        handle_syscall = sym handle_syscall_inner,
        user_rsp_offset = const SYSCALL_USER_RSP_SCRATCH_OFFSET,
    )
}

#[unsafe(no_mangle)]
unsafe extern "C" fn handle_syscall_inner(frame: *mut SyscallFrame) -> u64 {
    debug_assert!(frame as usize % 16 == 0);
    let frame = unsafe { &mut *frame };
    x86_64::instructions::interrupts::enable();
    {
        let core_id = get_current_core_id();
        crate::gdt::assert_rsp_in_task_stack(core_id);
    }

    let result = unsafe { dispatch_syscall(frame) };

    x86_64::instructions::interrupts::disable();
    scheduler::exit_to_user();

    result
}

unsafe fn dispatch_syscall(frame: &mut SyscallFrame) -> u64 {
    let syscall = unsafe { core::mem::transmute::<u64, SyscallNumber>(frame.syscall_num) };

    match syscall {
        SyscallNumber::CreateProcess => {
            serial_println_core!("CreateProcess: not implemented");
            SyscallError::NotImplemented.as_return()
        }
        SyscallNumber::TerminateProcess => {
            let target_pid = frame.arg1 as usize;
            let exit_code = frame.arg2 as i32;
            let caller_core = get_current_core_id();
            let caller_pid = scheduler::get_current_process_for_core(caller_core);

            if target_pid == INVALID_PID || target_pid == ARCHE_PID {
                serial_println_core!("TerminateProcess: invalid target pid {}", target_pid);
                return u64::MAX;
            }

            let target_parent = {
                let pm = PROCESS_MANAGER.lock();
                match pm.get_process(target_pid) {
                    Ok(p) => p.parent_pid,
                    Err(_) => {
                        serial_println_core!(
                            "TerminateProcess: target pid {} not found",
                            target_pid
                        );
                        return u64::MAX;
                    }
                }
            };

            let allowed =
                target_pid == caller_pid || target_parent == caller_pid || caller_pid == ARCHE_PID;
            if !allowed {
                serial_println_core!(
                    "TerminateProcess: pid {} denied to kill {} (parent {})",
                    caller_pid,
                    target_pid,
                    target_parent
                );
                return u64::MAX;
            }

            if target_pid == caller_pid {
                crate::process::task::exit_current(exit_code);
            }

            // the target tears itself down once it is back at the edge of ring 3
            if !crate::process::task::request_kill(target_pid, exit_code) {
                serial_println_core!("TerminateProcess: target {} has no task", target_pid);
                return u64::MAX;
            }

            serial_println_core!(
                "TerminateProcess: pid {} asked {} to exit with code {}",
                caller_pid,
                target_pid,
                exit_code
            );
            0
        }
        SyscallNumber::Write => {
            let fd = frame.arg1;
            let buf = frame.arg2 as *const u8;
            let count = frame.arg3 as usize;
            let slice = unsafe { core::slice::from_raw_parts(buf, count) };
            if let Ok(s) = core::str::from_utf8(slice) {
                if fd == 1 || fd == 2 {
                    let core_id = get_current_core_id();
                    let pid = scheduler::get_current_process_for_core(core_id);
                    crate::serial2_print!("[pid={}] {}", pid, s);
                } else {
                    serial_println_core!("WRITE: fd={}, count={}: {}", fd, count, s);
                }
            }
            count as u64
        }
        SyscallNumber::CreateWindow => {
            let width = frame.arg1 as u32;
            let height = frame.arg2 as u32;
            let x = frame.arg3 as i32;
            let y = frame.arg4 as i32;

            let core_id = get_current_core_id();
            let pid = scheduler::get_current_process_for_core(core_id);

            let compositor = crate::graphics::compositor::get_compositor();
            if width == 0
                || height == 0
                || width > compositor.framebuffer_width()
                || height > compositor.framebuffer_height()
            {
                serial_println_core!(
                    "sys_create_window: {}x{} exceeds framebuffer {}x{}",
                    width,
                    height,
                    compositor.framebuffer_width(),
                    compositor.framebuffer_height()
                );
                return u64::MAX;
            }

            let (window_id, _buffer, _event_buffer) =
                compositor.create_window(width, height, x, y, pid);
            serial_println_core!(
                "sys_create_window: {}x{} at ({},{}) -> id={}",
                width,
                height,
                x,
                y,
                window_id
            );
            compositor.set_z_index(window_id, 7);
            drop(compositor);

            window_id as u64
        }
        SyscallNumber::DestroyWindow => {
            let window_id = frame.arg1 as u32;
            crate::graphics::compositor::get_compositor().destroy_window(window_id);
            serial_println_core!("sys_destroy_window: id={}", window_id);
            0
        }
        SyscallNumber::GetPID => {
            let core_id = get_current_core_id();
            let pid = scheduler::get_current_process_for_core(core_id);
            pid as u64
        }
        SyscallNumber::Yield => {
            if SYSCALL_DEBUG_PRINT {
                let core_id = get_current_core_id();
                let pid = scheduler::get_current_process_for_core(core_id);
                serial_println_core!("sys_yield: PID {} yielding", pid);
            }

            scheduler::core_yield_executing_tasks(get_current_core_id());
            0
        }
        SyscallNumber::Exit => {
            let exit_code = frame.arg1;
            let core_id = get_current_core_id();
            let pid = scheduler::get_current_process_for_core(core_id);
            serial_println_core!("sys_exit: PID {} exiting with code {}", pid, exit_code);

            crate::process::task::exit_current(exit_code as i32);
        }
        SyscallNumber::Allocate => {
            let size = frame.arg1 as usize;
            let core_id = get_current_core_id();
            let pid = scheduler::get_current_process_for_core(core_id);
            let umm = crate::memory::get_user_mem_mgr();
            let mut pm = PROCESS_MANAGER.lock();
            let mut fa = crate::memory::get_frame_allocator();
            match pm.get_process_mut(pid) {
                Ok(proc) => match proc.memory_layout.allocate_heap(size, umm, &mut fa) {
                    Ok(old_ptr) => {
                        serial_println_core!(
                            "sys_allocate: pid={} size={} -> ptr={:#x}",
                            pid,
                            size,
                            old_ptr.as_u64()
                        );
                        old_ptr.as_u64()
                    }
                    Err(e) => {
                        serial_println_core!(
                            "sys_allocate: pid={} size={} allocate_heap failed: {:?}",
                            pid,
                            size,
                            e
                        );
                        u64::MAX
                    }
                },
                Err(_) => {
                    serial_println_core!("sys_allocate: pid={} not found", pid);
                    u64::MAX
                }
            }
        }
        SyscallNumber::MapWindowBuffer => {
            let window_id = frame.arg1 as u32;
            let core_id = get_current_core_id();
            let pid = scheduler::get_current_process_for_core(core_id);

            // Retrieve the WindowBuffer Arc from the compositor, then release the compositor lock.
            let (buffer_arc, max_window_buffer_size, user_base) = {
                let compositor = crate::graphics::compositor::get_compositor();
                let max_window_buffer_size = compositor.max_window_buffer_bytes();
                let user_base = compositor.window_buffer_user_base(window_id);
                let windows = compositor.windows.read();
                match windows.get(window_id as usize) {
                    Some(w) if w.is_visible && w.owner_pid == pid => (
                        alloc::sync::Arc::clone(&w.buffer),
                        max_window_buffer_size,
                        user_base,
                    ),
                    _ => {
                        serial_println_core!(
                            "sys_map_window_buffer: window_id={} not found",
                            window_id
                        );
                        return u64::MAX;
                    }
                }
            };

            let back_vaddr = buffer_arc.back_buffer_virt_addr();
            let front_vaddr = buffer_arc.front_buffer_virt_addr();
            let byte_count = buffer_arc.pixel_count() * FRAMEBUFFER_BYTES_PER_PIXEL as usize;
            if byte_count as u64 > max_window_buffer_size {
                serial_println_core!(
                    "sys_map_window_buffer: window_id={} buffer size {} exceeds slot max {}",
                    window_id,
                    byte_count,
                    max_window_buffer_size
                );
                return u64::MAX;
            }
            let page_count = buffer_arc.page_count();

            let umm = crate::memory::get_user_mem_mgr();
            let flags = x86_64::structures::paging::PageTableFlags::PRESENT
                | x86_64::structures::paging::PageTableFlags::WRITABLE
                | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;

            let mut back_frames: alloc::vec::Vec<x86_64::PhysAddr> =
                alloc::vec::Vec::with_capacity(page_count);
            let mut front_frames: alloc::vec::Vec<x86_64::PhysAddr> =
                alloc::vec::Vec::with_capacity(page_count);
            for i in 0..page_count {
                let page_offset = (i * PAGE_SIZE) as u64;
                back_frames.push(umm.translate_kernel_heap_virt_to_phys(back_vaddr + page_offset));
                front_frames
                    .push(umm.translate_kernel_heap_virt_to_phys(front_vaddr + page_offset));
            }

            let back_user_virt = x86_64::VirtAddr::new(user_base);
            let front_user_virt = x86_64::VirtAddr::new(user_base + max_window_buffer_size);
            let buffer_bytes = (page_count * PAGE_SIZE) as u64;

            {
                let mut pm = PROCESS_MANAGER.lock();
                let mut frame_allocator = get_frame_allocator();
                let Ok(proc) = pm.get_process_mut(pid) else {
                    serial_println_core!("sys_map_window_buffer: pid={} not found", pid);
                    return u64::MAX;
                };
                let layout = &mut proc.memory_layout;

                // the kernel swaps its front and back buffers, so mapping again would not match
                if layout.is_range_tracked(back_user_virt, buffer_bytes)
                    && layout.is_range_tracked(front_user_virt, buffer_bytes)
                {
                    return user_base;
                }

                if let Err(e) = layout.map_frames(
                    back_user_virt,
                    &back_frames,
                    flags,
                    FrameOwnership::Borrowed,
                    umm,
                    &mut frame_allocator,
                ) {
                    serial_println_core!(
                        "sys_map_window_buffer: mapping back buffer failed: {:?}",
                        e
                    );
                    return u64::MAX;
                }
                if let Err(e) = layout.map_frames(
                    front_user_virt,
                    &front_frames,
                    flags,
                    FrameOwnership::Borrowed,
                    umm,
                    &mut frame_allocator,
                ) {
                    serial_println_core!(
                        "sys_map_window_buffer: mapping front buffer failed: {:?}",
                        e
                    );
                    layout.unmap_region(back_user_virt, buffer_bytes, umm, &mut frame_allocator);
                    return u64::MAX;
                }
            }

            serial_println_core!(
                "sys_map_window_buffer: window_id={} mapped {} pages at user {:#x}",
                window_id,
                page_count,
                user_base
            );
            user_base
        }
        SyscallNumber::PresentWindow => {
            let window_id = frame.arg1 as u32;
            let compositor = crate::graphics::compositor::get_compositor();
            let windows = compositor.windows.read();
            if let Some(w) = windows.get(window_id as usize) {
                if w.is_visible {
                    w.buffer.present();
                }
            }
            //serial_println_core!("sys_present_window: window_id={} presented", window_id);
            drop(windows);
            drop(compositor);
            0
        }
        SyscallNumber::GetWindowSize => {
            let window_id = frame.arg1 as u32;
            let compositor = crate::graphics::compositor::get_compositor();
            let windows = compositor.windows.read();
            match windows.get(window_id as usize) {
                Some(w) if w.is_visible => {
                    let width = w.buffer.width as u64;
                    let height = w.buffer.height as u64;
                    (width << 32) | height
                }
                _ => u64::MAX,
            }
        }
        SyscallNumber::GetWindowInfo => {
            let window_id = frame.arg1 as u32;
            let info_ptr = frame.arg2 as usize;

            serial_println_core!("syscall: GetWindowInfo: window_id={}", window_id);

            if !validate_user_ptr(info_ptr, core::mem::size_of::<WindowInfo>()) {
                return FileDescriptor::INVALID_FD;
            }

            let compositor = crate::graphics::compositor::get_compositor();
            let windows = compositor.windows.read();
            match windows.get(window_id as usize) {
                Some(w) => {
                    let out = unsafe { &mut *(info_ptr as *mut WindowInfo) };
                    out.width = w.buffer.width;
                    out.height = w.buffer.height;
                    out.event_buffer_vaddr = match w.event_buffer {
                        Some(_) => event_buffer_user_vaddr(window_id),
                        None => 0,
                    };
                    out.front_buffer_offset = compositor.max_window_buffer_bytes();
                }
                _ => {
                    return u64::MAX;
                }
            }

            0
        }
        SyscallNumber::FocusWindow => {
            let window_id = frame.arg1 as u32;
            crate::graphics::compositor::get_compositor().focus_window(window_id);
            serial_println_core!("sys_focus_window: id={}", window_id);
            0
        }

        file_syscall @ (SyscallNumber::OpenFile
        | SyscallNumber::CloseFile
        | SyscallNumber::ReadFile
        | SyscallNumber::WriteFile
        | SyscallNumber::StatFile
        | SyscallNumber::ListDir
        | SyscallNumber::CreateFile
        | SyscallNumber::CreateDir
        | SyscallNumber::Delete
        | SyscallNumber::PinFile
        | SyscallNumber::UnpinFile
        | SyscallNumber::ReserveCache
        | SyscallNumber::EvictDirectory
        | SyscallNumber::GetCacheStats
        | SyscallNumber::FlushFileCache) => {
            serial_println_core!("{:?}: not implemented", file_syscall);
            SyscallError::NotImplemented.as_return()
        }

        SyscallNumber::CreateCircularBuffer => {
            let size_bytes = frame.arg1 as usize;
            let page_flags = PageTableFlags::from_bits_truncate(frame.arg2);
            let buffer_info_out = frame.arg3 as *mut CircularBufferInfo;

            if !validate_user_ptr(
                buffer_info_out as usize,
                core::mem::size_of::<CircularBufferInfo>(),
            ) {
                serial_println_core!("CreateCircularBuffer: invalid buffer_info_out pointer");
                return u64::MAX;
            }

            let core_id = get_current_core_id();
            let pid = scheduler::get_current_process_for_core(core_id);

            let user_memory_manager = crate::memory::get_user_mem_mgr();
            let mut pm = crate::process::process_manager::PROCESS_MANAGER.lock();
            let mut frame_allocator = get_frame_allocator();

            if let Ok(proc) = pm.get_process_mut(pid) {
                serial_println_core!("CreateCircularBuffer: found process for pid: {}", pid);

                match CircularBuffer::map_for_user(
                    size_bytes,
                    page_flags as PageTableFlags,
                    user_memory_manager,
                    &mut proc.memory_layout,
                    &mut frame_allocator,
                ) {
                    Some(buffer_info) => {
                        serial_println_core!(
                            "CreateCircularBuffer: buffer created, virt_base: {}, view_size: {}",
                            buffer_info.virtual_base,
                            buffer_info.view_size
                        );
                        buffer_info_out.write_unaligned(buffer_info);
                        return 0;
                    }
                    None => {
                        serial_println_core!("CreateCircularBuffer: cant create the buffer");
                        return u64::MAX;
                    }
                }
            }

            u64::MAX
        }

        SyscallNumber::GetCpuInfo => {
            let buffer_ptr = frame.arg1 as usize;
            let buffer_len = frame.arg2 as usize;

            if !validate_user_ptr(buffer_ptr, buffer_len) {
                serial_println_core!("sys_get_cpu_info: invalid pointer");
                return 2;
            }

            let core_id = get_current_core_id();
            let cpu_info = get_cpu_info_for_core(core_id);

            let tsc_freq = TSC_FREQUENCY_HZ.load(Ordering::Relaxed);
            let boot_tsc = BOOT_TSC.load(Ordering::Relaxed);
            let core_count = unsafe { TOTAL_CORE_COUNT };

            let flat = CpuInfoFlat::from_kernel_info(cpu_info, tsc_freq, boot_tsc, core_count);

            unsafe {
                core::ptr::write(buffer_ptr as *mut CpuInfoFlat, flat);
            }

            0
        }

        SyscallNumber::SleepMs => {
            crate::process::wait::sleep_current_task_ms(frame.arg1);
            0
        }
        SyscallNumber::WaitForInput => {
            let core_id = get_current_core_id();
            let pid = scheduler::get_current_process_for_core(core_id);
            crate::process::wait::block_current_task_until_event(
                &crate::process::wait::INPUT_WAIT_QUEUE,
                || crate::graphics::compositor::get_compositor().process_has_pending_input(pid),
            );
            0
        }
        SyscallNumber::FutexWait => {
            let word_ptr = frame.arg1 as usize;
            let expected = frame.arg2 as u32;
            if !validate_user_ptr(word_ptr, core::mem::size_of::<u32>()) {
                return u64::MAX;
            }

            // blocks while the word still holds the expected value
            crate::process::wait::block_current_task_until_event(
                &crate::process::wait::FUTEX_WAIT_QUEUE,
                || core::ptr::read_volatile(word_ptr as *const u32) != expected,
            );
            0
        }
        SyscallNumber::FutexWake => {
            crate::process::wait::FUTEX_WAIT_QUEUE.wake_all();
            0
        }

        _ => u64::MAX,
    }
}

pub fn init_syscall() {
    let core_id = get_current_core_id() as usize;
    debug_assert!(core_id < crate::MAX_CORES as usize);

    let slot_addr = unsafe { core::ptr::addr_of!(SYSCALL_DATA[core_id]) as u64 };
    unsafe {
        msr_write(MSR_KERNEL_GS_BASE, slot_addr);
    }

    unsafe {
        Efer::update(|flags| {
            flags.insert(EferFlags::SYSTEM_CALL_EXTENSIONS);
        });
    }

    // STAR MSR layout:
    // https://www.felixcloutier.com/x86/sysret
    // Bits 63:48 = User CS base for sysretq
    // Bits 47:32 = Kernel CS base for syscall
    //
    // GDT layout:
    //   0x08 = kernel code, 0x10 = kernel data
    //   0x18 = user code, 0x20 = user data
    //
    // sysretq sets CS = (STAR[63:48] + 16) | 3 = 0x23, SS = (STAR[63:48] + 8) | 3 = 0x1B
    let star_value = (0x10u64 << 48) | (0x08u64 << 32);
    let sfmask: u64 = 1 << 9;
    unsafe {
        msr_write(0xC0000081, star_value);
        msr_write(0xC0000082, syscall_handler as *const () as u64);
        msr_write(0xC0000084, sfmask);
    }

    serial_println_core!("Syscall MSRs initialized");
}
