use crate::{
    HHDM_OFFSET,
    events::event_buffer::EventBuffer,
    memory::{
        get_frame_allocator,
        memory::{MemoryMapFrameAllocator, PAGE_SIZE},
        usermem::{self, UserMemoryManager},
    },
    process::{
        PID,
        process::INVALID_PID,
        process_mem::{FrameOwnership, ProcessMemoryLayout},
        shared_state,
    },
    serial_println, serial_println_core,
};
use core::sync::atomic::{AtomicU32, Ordering};
use spin::{Mutex, MutexGuard};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{PageTableFlags, Size4KiB, mapper::MapToError},
};

const SHARED_REGION_COUNT: usize = 1;

//const IO_EVENT_BUFFER_INDEX: usize = 0;
const PROGRAM_SHARED_DATA_INDEX: usize = 0;

// Event buffers are mapped per window, see event_buffer_user_vaddr. This address is now only the base from which the shared data page address is derived
pub const EVENT_BUFFER_ADDR: usize = 0x0000_0007_0000_0000;
pub const PROGRAM_SHARED_DATA_ADDR: usize = EVENT_BUFFER_ADDR + PAGE_SIZE;

pub const TEST_COUNTER_COUNT: usize = 16;

#[repr(C, align(4096))]
pub struct ProgramSharedDataBuffer {
    pub focused_window_id: AtomicU32,
    /// Scratch counters the kernel integration tests use to coordinate user programs
    pub test_counters: [AtomicU32; TEST_COUNTER_COUNT],
}

pub static SHARED_STATE: spin::Once<Mutex<SharedState>> = spin::Once::new();

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum SharedRegionType {
    Default = 0,
    EventBuffer = 1,
    ProgramSharedDataBuffer = 2,
}

#[derive(Clone, Copy)]
pub struct SharedRegion {
    pub phys_addr: PhysAddr,
    pub kernel_vaddr: VirtAddr,
    pub default_user_vaddr: VirtAddr,
    pub size_bytes: u32,
    pub region_type: SharedRegionType,
    pub _reserved: u8,
    pub page_table_flags: PageTableFlags,
}

pub struct SharedState {
    pub regions: [SharedRegion; SHARED_REGION_COUNT],
}

impl SharedRegion {
    pub const fn new() -> Self {
        SharedRegion {
            phys_addr: PhysAddr::zero(),
            kernel_vaddr: VirtAddr::zero(),
            default_user_vaddr: VirtAddr::zero(),
            size_bytes: 0,
            region_type: SharedRegionType::Default,
            _reserved: 0,
            page_table_flags: PageTableFlags::BIT_9,
        }
    }
}

impl SharedState {
    pub const fn new() -> Self {
        Self {
            regions: [SharedRegion::new(); SHARED_REGION_COUNT],
        }
    }

    pub fn register_region(&mut self, region_index: u8, region: SharedRegion) {
        debug_assert!(region_index < SHARED_REGION_COUNT as u8);
        self.regions[region_index as usize] = region;
    }

    pub fn map_into_process(
        &self,
        user_memory_manager: &UserMemoryManager,
        memory_layout: &mut ProcessMemoryLayout,
        frame_allocator: &mut MemoryMapFrameAllocator,
    ) -> Result<(), MapToError<Size4KiB>> {
        for i in 0..SHARED_REGION_COUNT {
            let region = self.regions[i];
            if crate::process::PROCESS_DEBUG_PRINT {
                serial_println_core!(
                    "map_into_process: Mapping {} to user vaddr {:#x}",
                    region.region_type as u8,
                    region.default_user_vaddr.as_u64()
                );
            }

            memory_layout.map_frames(
                region.default_user_vaddr,
                &[region.phys_addr],
                region.page_table_flags,
                FrameOwnership::Borrowed,
                user_memory_manager,
                frame_allocator,
            )?;
        }

        Ok(())
    }
}

//NOTE: Has to be called before creating any userspace processes
pub fn init_shared_state() {
    serial_println_core!("init_shared_state - begin");
    let mut shared_state = SharedState::new();

    //TODO: readonly for user, writable for kernel
    // match init_event_buffer() {
    //     Some((buffer_phys, buffer_virt)) => {
    //         shared_state.register_region(
    //             IO_EVENT_BUFFER_INDEX as u8,
    //             SharedRegion {
    //                 phys_addr: buffer_phys,
    //                 kernel_vaddr: buffer_virt,
    //                 default_user_vaddr: VirtAddr::new(EVENT_BUFFER_ADDR as u64),
    //                 size_bytes: PAGE_SIZE as u32,
    //                 region_type: SharedRegionType::EventBuffer,
    //                 _reserved: 0,
    //                 page_table_flags: PageTableFlags::PRESENT
    //                     | PageTableFlags::WRITABLE
    //                     | PageTableFlags::USER_ACCESSIBLE,
    //             },
    //         );
    //     }
    //     None => {}
    // }
    match init_program_shared_data_buffer() {
        Some((buffer_phys, buffer_virt)) => {
            shared_state.register_region(
                PROGRAM_SHARED_DATA_INDEX as u8,
                SharedRegion {
                    phys_addr: buffer_phys,
                    kernel_vaddr: buffer_virt,
                    default_user_vaddr: VirtAddr::new(PROGRAM_SHARED_DATA_ADDR as u64),
                    size_bytes: PAGE_SIZE as u32,
                    region_type: SharedRegionType::ProgramSharedDataBuffer,
                    _reserved: 0,
                    page_table_flags: PageTableFlags::PRESENT
                        | PageTableFlags::WRITABLE
                        | PageTableFlags::USER_ACCESSIBLE
                        | PageTableFlags::NO_EXECUTE,
                },
            );
        }
        None => {}
    }

    SHARED_STATE.call_once(|| Mutex::new(shared_state));

    serial_println_core!("init_shared_state: shared state initialized");
}

fn init_event_buffer() -> Option<(PhysAddr, VirtAddr)> {
    serial_println_core!("init_shared_state: init_event_buffer");
    let mut frame_allocator = get_frame_allocator();
    let (phys, kernel_vaddr) = match usermem::allocate_zeroed_page(&mut frame_allocator) {
        Some((phys, virt)) => (phys, virt),
        None => return None,
    };

    unsafe {
        let buffer = &*(kernel_vaddr.as_u64() as *const EventBuffer);
        assert_eq!(buffer.write_idx.load(Ordering::Relaxed), 0);
        assert_eq!(buffer.read_idx.load(Ordering::Relaxed), 0);
        assert_eq!(buffer.event_count.load(Ordering::Relaxed), 0);
    }

    serial_println!("EventBuffer initialized at phys {:?}", phys);

    Some((phys, kernel_vaddr))
}

fn init_program_shared_data_buffer() -> Option<(PhysAddr, VirtAddr)> {
    serial_println_core!("init_shared_state: init_program_shared_data_buffer");
    let mut frame_allocator = get_frame_allocator();
    let (phys, kernel_vaddr) = match usermem::allocate_zeroed_page(&mut frame_allocator) {
        Some((phys, virt)) => (phys, virt),
        None => return None,
    };
    serial_println!("ProgramSharedDataBuffer initialized at phys {:?}", phys);

    Some((phys, kernel_vaddr))
}

// pub unsafe fn get_shared_input_event_buffer() -> &'static EventBuffer {
//     let shared_state = SHARED_STATE.get().unwrap().lock();
//     let region = shared_state.regions[IO_EVENT_BUFFER_INDEX];
//     unsafe { &*(region.kernel_vaddr.as_u64() as *const EventBuffer) }
// }

pub unsafe fn get_shared_program_data_buffer() -> &'static ProgramSharedDataBuffer {
    let shared_state = SHARED_STATE.get().unwrap().lock();
    let region = shared_state.regions[PROGRAM_SHARED_DATA_INDEX];
    unsafe { &*(region.kernel_vaddr.as_u64() as *const ProgramSharedDataBuffer) }
}

pub unsafe fn get_shared_program_data_buffer_mut() -> &'static mut ProgramSharedDataBuffer {
    let shared_state = SHARED_STATE.get().unwrap().lock();
    let region = shared_state.regions[PROGRAM_SHARED_DATA_INDEX];
    unsafe { &mut *(region.kernel_vaddr.as_u64() as *mut ProgramSharedDataBuffer) }
}
