use core::arch::asm;
use core::cell::UnsafeCell;
use x86_64::VirtAddr;
use x86_64::registers::segmentation::Segment;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::paging::Mapper;
use x86_64::structures::tss::TaskStateSegment;

use crate::process::task::MAX_TASKS;
use crate::{MAX_CORES, serial_println, serial_println_core};

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

// Stack size for each per-core stack (RSP0 and IST).
pub const PER_CORE_STACK_SIZE: usize = 8 * 4096; // 32 KiB
pub const GUARD_PAGE_SIZE: usize = 4096;

// Syscalls run on the task stack, so it needs the room the old per-core syscall stack had
pub const TASK_KERNEL_STACK_SIZE: usize = 16 * 4096; // 64 KiB

pub const KERNEL_CODE_SELECTOR_VALUE: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR_VALUE: u16 = 0x10;
pub const USER_DATA_SELECTOR_VALUE: u16 = 0x18 | 3;
pub const USER_CODE_SELECTOR_VALUE: u16 = 0x20 | 3;

#[repr(C, align(4096))]
pub struct GuardedKernelStack<const STACK_SIZE: usize = PER_CORE_STACK_SIZE> {
    pub guard: [u8; GUARD_PAGE_SIZE],
    pub stack: [u8; STACK_SIZE],
}

impl<const STACK_SIZE: usize> GuardedKernelStack<STACK_SIZE> {
    const fn new() -> Self {
        Self {
            guard: [0u8; GUARD_PAGE_SIZE],
            stack: [0u8; STACK_SIZE],
        }
    }
}

static mut RSP0_STACKS: [GuardedKernelStack; MAX_CORES as usize] =
    [const { GuardedKernelStack::new() }; MAX_CORES as usize];

static mut IST0_STACKS: [GuardedKernelStack; MAX_CORES as usize] =
    [const { GuardedKernelStack::new() }; MAX_CORES as usize];

static mut SCHEDULER_STACKS: [GuardedKernelStack; MAX_CORES as usize] =
    [const { GuardedKernelStack::new() }; MAX_CORES as usize];

static mut KERNEL_TASK_STACKS: [GuardedKernelStack<TASK_KERNEL_STACK_SIZE>; MAX_TASKS] =
    [const { GuardedKernelStack::new() }; MAX_TASKS];

pub fn task_stack_top(slot_index: usize) -> u64 {
    debug_assert!(slot_index < MAX_TASKS);

    unsafe {
        let slot = &KERNEL_TASK_STACKS[slot_index];
        slot.stack.as_ptr().add(TASK_KERNEL_STACK_SIZE) as u64
    }
}

pub fn task_guard_page(slot_index: usize) -> VirtAddr {
    debug_assert!(slot_index < MAX_TASKS);

    unsafe {
        let slot = &KERNEL_TASK_STACKS[slot_index];
        VirtAddr::new(slot.guard.as_ptr() as u64)
    }
}

pub fn task_stack_bounds(slot_index: usize) -> (VirtAddr, VirtAddr) {
    debug_assert!(slot_index < MAX_TASKS);

    unsafe {
        let slot = &KERNEL_TASK_STACKS[slot_index];
        let bottom = VirtAddr::new(slot.stack.as_ptr() as u64);
        let top = VirtAddr::new(slot.stack.as_ptr().add(TASK_KERNEL_STACK_SIZE) as u64);
        (bottom, top)
    }
}

/// Where the CPU switches to on a ring 3 to ring 0 transition of this core
pub fn set_privilege_stack_top(core_id: u8, stack_top: u64) {
    let tss = unsafe { &mut *PER_CORE_TSS[core_id as usize].get() };
    tss.privilege_stack_table[0] = VirtAddr::new(stack_top);
}

pub fn get_scheduler_stack_top(core_id: u8) -> u64 {
    unsafe {
        let slot = &SCHEDULER_STACKS[core_id as usize];
        slot.stack.as_ptr().add(PER_CORE_STACK_SIZE) as u64
    }
}

pub fn rsp0_stack_top(core_id: u8) -> u64 {
    unsafe {
        let slot = &RSP0_STACKS[core_id as usize];
        slot.stack.as_ptr().add(PER_CORE_STACK_SIZE) as u64
    }
}

pub fn ist0_stack_top(core_id: u8) -> u64 {
    unsafe {
        let slot = &IST0_STACKS[core_id as usize];
        slot.stack.as_ptr().add(PER_CORE_STACK_SIZE) as u64
    }
}

pub fn rsp0_guard_page(core_id: u8) -> VirtAddr {
    unsafe {
        let slot = &RSP0_STACKS[core_id as usize];
        VirtAddr::new(slot.guard.as_ptr() as u64)
    }
}

pub fn ist0_guard_page(core_id: u8) -> VirtAddr {
    unsafe {
        let slot = &IST0_STACKS[core_id as usize];
        VirtAddr::new(slot.guard.as_ptr() as u64)
    }
}

pub fn scheduler_guard_page(core_id: u8) -> VirtAddr {
    unsafe {
        let slot = &SCHEDULER_STACKS[core_id as usize];
        VirtAddr::new(slot.guard.as_ptr() as u64)
    }
}

pub fn rsp0_bounds(core_id: u8) -> (VirtAddr, VirtAddr) {
    unsafe {
        let slot = &RSP0_STACKS[core_id as usize];
        let bottom = VirtAddr::new(slot.stack.as_ptr() as u64);
        let top = VirtAddr::new(slot.stack.as_ptr().add(PER_CORE_STACK_SIZE) as u64);
        (bottom, top)
    }
}

pub fn ist0_bounds(core_id: u8) -> (VirtAddr, VirtAddr) {
    unsafe {
        let slot = &IST0_STACKS[core_id as usize];
        let bottom = VirtAddr::new(slot.stack.as_ptr() as u64);
        let top = VirtAddr::new(slot.stack.as_ptr().add(PER_CORE_STACK_SIZE) as u64);
        (bottom, top)
    }
}

pub fn scheduler_bounds(core_id: u8) -> (VirtAddr, VirtAddr) {
    unsafe {
        let slot = &SCHEDULER_STACKS[core_id as usize];
        let bottom = VirtAddr::new(slot.stack.as_ptr() as u64);
        let top = VirtAddr::new(slot.stack.as_ptr().add(PER_CORE_STACK_SIZE) as u64);
        (bottom, top)
    }
}

pub fn install_guard_pages(frame_allocator: &mut crate::memory::memory::MemoryMapFrameAllocator) {
    for core_id in 0..MAX_CORES as usize {
        unsafe {
            crate::stack_guard::ensure_guard_unmapped(
                rsp0_guard_page(core_id as u8),
                frame_allocator,
            );
            crate::stack_guard::ensure_guard_unmapped(
                ist0_guard_page(core_id as u8),
                frame_allocator,
            );
            crate::stack_guard::ensure_guard_unmapped(
                scheduler_guard_page(core_id as u8),
                frame_allocator,
            );
        }
    }

    for slot_index in 0..MAX_TASKS {
        unsafe {
            crate::stack_guard::ensure_guard_unmapped(task_guard_page(slot_index), frame_allocator);
        }
    }
}

/// The running task owns its kernel stack, so rsp must lie inside it
#[inline(always)]
pub fn assert_rsp_in_task_stack(core_id: u8) {
    let slot_index = crate::process::scheduler::get_current_slot_for_core(core_id);
    if slot_index == crate::process::task::INVALID_SLOT as usize {
        return;
    }

    let rsp: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack)) };
    let (bottom, top) = task_stack_bounds(slot_index);
    debug_assert!(
        rsp >= bottom.as_u64() && rsp <= top.as_u64(),
        "RSP {:#x} out of bounds for task slot {} on core {} [{:#x}..{:#x}]",
        rsp,
        slot_index,
        core_id,
        bottom.as_u64(),
        top.as_u64()
    );
}

static mut PER_CORE_GDT: [Gdt; MAX_CORES as usize] = {
    const EMPTY: Gdt = Gdt::empty();
    [EMPTY; MAX_CORES as usize]
};

struct Gdt {
    table: GlobalDescriptorTable,
    kernel_code_selector: SegmentSelector,
    kernel_data_selector: SegmentSelector,
    user_code_selector: SegmentSelector,
    user_data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

static mut PER_CORE_TSS: [UnsafeCell<TaskStateSegment>; MAX_CORES as usize] =
    unsafe { [const { UnsafeCell::new(TaskStateSegment::new()) }; MAX_CORES as usize] };

impl Gdt {
    const fn empty() -> Self {
        let mut table = GlobalDescriptorTable::new();
        let kernel_code_selector = table.append(Descriptor::kernel_code_segment());
        let kernel_data_selector = table.append(Descriptor::kernel_data_segment());
        let user_data_selector = table.append(Descriptor::user_data_segment());
        let user_code_selector = table.append(Descriptor::user_code_segment());
        let tss_selector = table.append(Descriptor::kernel_code_segment());

        Gdt {
            table,
            kernel_code_selector,
            kernel_data_selector,
            user_code_selector,
            user_data_selector,
            tss_selector,
        }
    }

    fn new(core_id: u8) -> Self {
        let idx = core_id as usize;
        let tss = unsafe { &mut *PER_CORE_TSS[idx].get() };

        let rsp0_top = rsp0_stack_top(core_id);
        tss.privilege_stack_table[0] = VirtAddr::new(rsp0_top);

        let ist0_top = ist0_stack_top(core_id);
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = VirtAddr::new(ist0_top);

        serial_println!(
            "Core {}: TSS RSP0={:#x}  IST0={:#x}",
            core_id,
            rsp0_top,
            ist0_top
        );

        let mut table = GlobalDescriptorTable::new();
        let kernel_code_selector = table.append(Descriptor::kernel_code_segment());
        let kernel_data_selector = table.append(Descriptor::kernel_data_segment());
        let user_data_selector = table.append(Descriptor::user_data_segment());
        let user_code_selector = table.append(Descriptor::user_code_segment());
        let tss_selector = table.append(Descriptor::tss_segment(tss));
        debug_assert!(kernel_code_selector.0 == KERNEL_CODE_SELECTOR_VALUE);
        debug_assert!(kernel_data_selector.0 == KERNEL_DATA_SELECTOR_VALUE);
        debug_assert!(user_data_selector.0 | 3 == USER_DATA_SELECTOR_VALUE);
        debug_assert!(user_code_selector.0 | 3 == USER_CODE_SELECTOR_VALUE);
        serial_println!("Initialized GDT for core {} with: kernel_code_selector={:#x}, kernel_data_selector={:#x}, 
        user_code_selector={:#x}, user_data_selector={:#x}, tss_selector={:#x}", core_id, kernel_code_selector.0, kernel_data_selector.0, 
        user_code_selector.0, user_data_selector.0, tss_selector.0);

        Gdt {
            table,
            kernel_code_selector,
            kernel_data_selector,
            user_data_selector,
            user_code_selector,
            tss_selector,
        }
    }
}

pub unsafe fn init_core_gdt(core_id: u8) {
    let gdt = Gdt::new(core_id);

    let idx = core_id as usize;
    if idx < MAX_CORES as usize {
        unsafe {
            PER_CORE_GDT[idx] = gdt;
        }
    }

    let gdt_ref = unsafe { &PER_CORE_GDT[idx] };

    gdt_ref.table.load();

    // Reload segments
    unsafe {
        asm!(
            "push {sel}",
            "lea {tmp}, [2f + rip]",
            "push {tmp}",
            "retfq",
            "2:",
            sel = in(reg) gdt_ref.kernel_code_selector.0 as u64,
            tmp = in(reg) 0u64,
            options(nostack)
        );

        x86_64::registers::segmentation::DS::set_reg(gdt_ref.kernel_data_selector);
        x86_64::registers::segmentation::ES::set_reg(gdt_ref.kernel_data_selector);
        x86_64::registers::segmentation::SS::set_reg(gdt_ref.kernel_data_selector);

        x86_64::instructions::tables::load_tss(gdt_ref.tss_selector);
    }
}

pub fn get_kernel_code_selector(core_id: u8) -> SegmentSelector {
    unsafe { PER_CORE_GDT[core_id as usize].kernel_code_selector }
}

pub fn get_kernel_data_selector(core_id: u8) -> SegmentSelector {
    unsafe { PER_CORE_GDT[core_id as usize].kernel_data_selector }
}

pub fn get_user_code_selector(core_id: u8) -> SegmentSelector {
    unsafe { PER_CORE_GDT[core_id as usize].user_code_selector }
}

pub fn get_user_data_selector(core_id: u8) -> SegmentSelector {
    unsafe { PER_CORE_GDT[core_id as usize].user_data_selector }
}

pub fn get_tss_selector(core_id: u8) -> SegmentSelector {
    unsafe { PER_CORE_GDT[core_id as usize].tss_selector }
}
