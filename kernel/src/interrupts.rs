#![allow(unused)]
use crate::events::event_buffer::{InputEvent, KeyModifiers, KeyState, Keys};
use crate::graphics::compositor;
use crate::io::serial;
use crate::process::process::DEFAULT_NEW_PROCESS_STACK_SIZE;
use crate::process::{
    process::INVALID_PID,
    process_manager::{ARCHE_PID, PROCESS_MANAGER},
};
use crate::serial_println_core;
use crate::trap::{TrapFrame, interrupt_stub, is_user_frame};
use crate::util::apic::APICOffset;
use crate::util::cpuinfo::get_current_core_id;
use crate::util::msr::{msr_read, msr_write};
use crate::{
    gdt, hlt_loop,
    memory::memory::{IdentityAcpiHandler, MemoryMapFrameAllocator},
    serial_print, serial_println,
    util::cpuinfo::{CpuFeatureFlags, get_cpu_info_for_core},
};
use acpi::{
    AcpiTables, PhysicalMapping,
    platform::interrupt::{Apic, InterruptModel, IoApic},
    platform::{AcpiMode, AcpiPlatform},
    sdt::{
        Signature,
        madt::{
            InterruptSourceOverrideEntry, IoApicEntry, LocalApicEntry, Madt, MadtEntry,
            NmiSourceEntry, PlatformInterruptSourceEntry,
        },
    },
};
use core::arch::asm;
use core::arch::x86_64::__rdtscp;
use core::sync::atomic::{AtomicPtr, AtomicU8, AtomicU64, Ordering};
use lazy_static::lazy_static;
use pc_keyboard::KeyCode;
#[cfg(feature = "ps2_mouse")]
use ps2_mouse::{Mouse, MouseState};
use spin::{Mutex, Once, lazy};
use x86_64::instructions::port::Port;
use x86_64::structures::paging::frame;
use x86_64::{
    PhysAddr, VirtAddr,
    registers::rflags::RFlags,
    structures::{
        idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode},
        paging::{FrameAllocator, Mapper, Page, PageTableFlags, PhysFrame, Size4KiB},
    },
};
#[cfg(feature = "ps2_mouse")]
lazy_static! {
    static ref MOUSE: Mutex<Mouse> = Mutex::new(Mouse::new());
}

const TIMER_DEBUG_PRINT: bool = false;
const KEYBOARD_DEBUG_PRINT: bool = false;
#[cfg(feature = "ps2_mouse")]
const MOUSE_DEBUG_PRINT: bool = false;
const TIMER_ENABLED: bool = true;

/// TSC frequency measured at boot via PIT calibration
/// Written once by core 0 before any AP is started
/// All cores read this after it is set
pub static TSC_FREQUENCY_HZ: AtomicU64 = AtomicU64::new(0);

/// TSC value at the moment core 0 finished basic init (just before APs start)
/// Used as the zero-point for log timestamps
pub static BOOT_TSC: AtomicU64 = AtomicU64::new(0);

pub const TIMER_TICK_INTERVAL_MS: u64 = 10;
pub const TIMER_TICK_FREQ_HZ: u64 = 1000 / TIMER_TICK_INTERVAL_MS;

const MSR_IA32_TSC_DEADLINE: u32 = 0x6E0;

static SYSTEM_TICKS: AtomicU64 = AtomicU64::new(0);
pub const TICK_DURATION_NS: u64 = 1_000_000_000 / TIMER_TICK_FREQ_HZ;
pub const TICK_DURATION_US: u64 = 1_000_000 / TIMER_TICK_FREQ_HZ;

const LAPIC_VIRT_BASE: u64 = 0xFFFF_FFFF_0000_0000;
const IOAPIC_VIRT_BASE: u64 = 0xFFFF_FFFF_FF00_0000;

const PIT_CHANNEL2_DATA: u16 = 0x42;
const PIT_CMD: u16 = 0x43;
const PIT_PC_SPEAKER: u16 = 0x61;
const PIT_BASE_HZ: u64 = 1_193_182;

const KEYBOARD_IRQ: u32 = 1;
#[cfg(feature = "ps2_mouse")]
const MOUSE_IRQ: u32 = 12;

/// Measure the TSC frequency by counting cycles over a PIT-timed interval
/// Must be called with interrupts disabled
pub unsafe fn measure_tsc_frequency_via_pit() -> u64 {
    // We count down from 65535 ticks of the PIT (around 54.9 ms)
    // Actual elapsed time = count / PIT_BASE_HZ seconds
    const PIT_COUNT: u16 = 0xFFFF;
    const PIT_MODE_ONE_SHOT_CH2: u8 = 0b1011_0000; // channel 2, lo/hi, mode 1

    unsafe {
        use x86_64::instructions::port::Port;

        let mut cmd_port: Port<u8> = Port::new(PIT_CMD);
        let mut ch2_port: Port<u8> = Port::new(PIT_CHANNEL2_DATA);
        let mut speaker_port: Port<u8> = Port::new(PIT_PC_SPEAKER);

        // Disable the PC speaker gate so the PIT runs silently.
        let old_speaker = speaker_port.read();
        // Bit 0 = gate input to channel 2; bit 1 = speaker output enable.
        // Set gate (bit 0), clear speaker (bit 1).
        speaker_port.write((old_speaker & !0x02) | 0x01);

        // Program channel 2 as one-shot.
        cmd_port.write(PIT_MODE_ONE_SHOT_CH2);

        // Load the count (LSB first, then MSB).
        ch2_port.write((PIT_COUNT & 0xFF) as u8);
        ch2_port.write((PIT_COUNT >> 8) as u8);

        // Read TSC right as we start.
        let tsc_start = tsc_read();

        // Poll bit 5 of port 0x61 (OUT2 status) until PIT reaches zero.
        // OUT2 goes high when the count expires.
        loop {
            let status = speaker_port.read();
            if status & 0x20 != 0 {
                break;
            }
        }

        let tsc_end = tsc_read();

        // Restore speaker port.
        speaker_port.write(old_speaker);

        // tsc_cycles elapsed over PIT_COUNT PIT ticks.
        let tsc_cycles = tsc_end - tsc_start;

        // TSC Hz = tsc_cycles * PIT_BASE_HZ / PIT_COUNT
        let freq = tsc_cycles * PIT_BASE_HZ / PIT_COUNT as u64;

        serial_println!(
            "TSC calibration: {} cycles over {} PIT ticks -> {} Hz ({} MHz)",
            tsc_cycles,
            PIT_COUNT,
            freq,
            freq / 1_000_000
        );

        freq
    }
}

/// Initialise the global TSC frequency and boot epoch
/// Have to call this on core 0 once, before starting APs, with interrupts disabled
pub unsafe fn init_tsc_globals() {
    let freq = unsafe { measure_tsc_frequency_via_pit() };
    TSC_FREQUENCY_HZ.store(freq, Ordering::SeqCst);
    // Record the epoch *after* calibration so t=0 is close to the point
    // where meaningful kernel work begins.
    BOOT_TSC.store(unsafe { tsc_read() }, Ordering::SeqCst);
    serial_println!(
        "TSC globals: freq={} Hz, boot_tsc={}",
        freq,
        BOOT_TSC.load(Ordering::Relaxed)
    );
}

/// Microseconds since `init_tsc_globals` was called on core 0
/// Safe to call from any core
#[inline]
pub fn tsc_timestamp_us() -> u64 {
    let freq = TSC_FREQUENCY_HZ.load(Ordering::Relaxed);
    if freq == 0 {
        return 0;
    }
    let boot = BOOT_TSC.load(Ordering::Relaxed);
    let now = unsafe { tsc_read() };
    now.saturating_sub(boot) / (freq / 1_000_000)
}

/// TSC cycles per timer tick, computed from the measured frequency
/// Falls back to a sane default until `init_tsc_globals` is called
pub fn tsc_cycles_per_tick() -> u64 {
    let freq = TSC_FREQUENCY_HZ.load(Ordering::Relaxed);
    if freq == 0 {
        // Fallback before calibration: assume 2 GHz, 10 ms tick
        return 2_000_000_000 / TIMER_TICK_FREQ_HZ;
    }
    freq / TIMER_TICK_FREQ_HZ
}

pub fn system_uptime_ns() -> u64 {
    SYSTEM_TICKS.load(core::sync::atomic::Ordering::Relaxed) * TICK_DURATION_NS
}

pub fn system_uptime_us() -> u64 {
    SYSTEM_TICKS.load(core::sync::atomic::Ordering::Relaxed) * TICK_DURATION_US
}

/// Virtual address of the memory mapped local APIC, identical on every core
/// Atomic instead of a lock because the timer interrupt reads it while the
/// interrupted code may be reading it too
pub static LAPIC_ADDRESS: AtomicPtr<u32> = AtomicPtr::new(core::ptr::null_mut());

/// LAPIC id of every core, filled at boot so sending an IPI needs no lock
static LAPIC_ID_OF_CORE: [AtomicU8; crate::MAX_CORES as usize] =
    [const { AtomicU8::new(0) }; crate::MAX_CORES as usize];

const ICR_LEVEL_ASSERT: u32 = 1 << 14;
const ICR_DELIVERY_PENDING: u32 = 1 << 12;
const ICR_DESTINATION_SHIFT: u32 = 24;

pub fn set_lapic_id_of_core(core_id: u8, lapic_id: u8) {
    LAPIC_ID_OF_CORE[core_id as usize].store(lapic_id, Ordering::Release);
}

pub fn send_reschedule_ipi(core_id: u8) {
    let lapic = LAPIC_ADDRESS.load(Ordering::Acquire);
    debug_assert!(!lapic.is_null());

    let destination_lapic_id = LAPIC_ID_OF_CORE[core_id as usize].load(Ordering::Acquire) as u32;

    // the two ICR halves must not be split by an interrupt that sends an IPI itself
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        let icr_low = lapic.offset(APICOffset::Icr1 as isize / 4);
        let icr_high = lapic.offset(APICOffset::Icr2 as isize / 4);

        while icr_low.read_volatile() & ICR_DELIVERY_PENDING != 0 {
            core::hint::spin_loop();
        }

        icr_high.write_volatile(destination_lapic_id << ICR_DESTINATION_SHIFT);
        icr_low.write_volatile(InterruptIndex::Reschedule as u32 | ICR_LEVEL_ASSERT);
    });
}

pub fn load_idt() {
    //serial_println!("load_idt");
    IDT.load();
}

fn disable_pic() {
    use x86_64::instructions::port::Port;
    unsafe {
        // mask all IRQs on both master (0x21) and slave (0xA1) PICs
        Port::<u8>::new(0x21).write(0xFF);
        Port::<u8>::new(0xA1).write(0xFF);
    }
}

pub unsafe fn interrupt_over() {
    unsafe {
        let local_apic_ptr = LAPIC_ADDRESS.load(Ordering::Acquire);
        local_apic_ptr
            .offset(APICOffset::Eoi as isize / 4)
            .write_volatile(0);
    }
}

unsafe fn map_apic_mem_identity(
    phys_address: u32,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> VirtAddr {
    let physical_address = PhysAddr::new(phys_address as u64);
    let page = Page::containing_address(VirtAddr::new(physical_address.as_u64()));
    let frame = PhysFrame::containing_address(physical_address);
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;

    serial_println!(
        "Mapping: phys {:#x}, virt: {:#x}",
        physical_address,
        page.start_address()
    );
    unsafe {
        mapper
            .map_to(page, frame, flags, frame_allocator)
            .expect("Mapping failed")
            .flush();
    }

    page.start_address()
}

unsafe fn map_apic_mem(
    phys_address: u32,
    base_virt_address: u64,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> VirtAddr {
    let physical_address = PhysAddr::new(phys_address as u64);
    let page = Page::containing_address(VirtAddr::new(base_virt_address));
    let frame = PhysFrame::containing_address(physical_address);
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;

    serial_println!(
        "Mapping: phys {:#x}, virt: {:#x}",
        physical_address,
        page.start_address()
    );
    unsafe {
        mapper
            .map_to(page, frame, flags, frame_allocator)
            .expect("Mapping failed")
            .flush();
    }

    page.start_address()
}

unsafe fn init_io_apic(
    phys_address: u32,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    serial_println!("Mapping IO APIC");

    let virt_addr =
        unsafe { map_apic_mem(phys_address, IOAPIC_VIRT_BASE, mapper, frame_allocator) };

    let io_apic_ptr = virt_addr.as_mut_ptr::<u32>();

    unsafe {
        const IOAPIC_ID_REG: u32 = 0x00;
        const IOAPIC_VER_REG: u32 = 0x01;
        io_apic_ptr.offset(0).write_volatile(IOAPIC_ID_REG);
        let ioapic_id = io_apic_ptr.offset(4).read_volatile() >> 24;
        serial_println!("IO APIC ID: {}", ioapic_id);

        io_apic_ptr.offset(0).write_volatile(IOAPIC_VER_REG);
        let version = io_apic_ptr.offset(4).read_volatile() & 0xFF;
        let max_redir_entry = ((io_apic_ptr.offset(4).read_volatile() >> 16) & 0xFF) + 1;
        serial_println!(
            "IO APIC version: {}, max redirection entries: {}",
            version,
            max_redir_entry
        );

        set_ioapic_redirection(
            io_apic_ptr,
            KEYBOARD_IRQ,
            InterruptIndex::Keyboard as u8,
            false,
        );

        #[cfg(feature = "ps2_mouse")]
        set_ioapic_redirection(io_apic_ptr, MOUSE_IRQ, InterruptIndex::Mouse as u8, false);
    }
}

unsafe fn set_ioapic_redirection(
    io_apic_ptr: *mut u32,
    irq: u32,
    vector: u8,
    level_triggered: bool,
) {
    let entry_low = 0x10 + (irq * 2);
    let entry_high = 0x10 + (irq * 2 + 1);

    let mut low_value = vector as u32;
    if level_triggered {
        low_value |= 1 << 15;
    }
    // Bit 16: 0 = enabled, 1 = masked
    // Bit 11: destination mode: 0 = physical, 1 = logical
    // Bits 8-10: delivery mode: 000 = fixed
    // Bits 24-27: destination field: APIC ID of target CPU
    let high_value = 0u32; // Send to APIC ID 0 (BSP)

    io_apic_ptr.offset(0).write_volatile(entry_low);
    io_apic_ptr.offset(4).write_volatile(low_value);

    io_apic_ptr.offset(0).write_volatile(entry_high);
    io_apic_ptr.offset(4).write_volatile(high_value);

    io_apic_ptr.offset(0).write_volatile(entry_low);
    let readback_low = io_apic_ptr.offset(4).read_volatile();

    serial_println!(
        "IO APIC IRQ {}: wrote {:#x}, readback {:#x} (vector: {}, enabled: {})",
        irq,
        low_value,
        readback_low,
        readback_low & 0xFF,
        (readback_low >> 16) & 1 == 0,
    );
}

pub unsafe fn init_lapic_for_current_core(core_id: u8) {
    let lapic_ptr = get_lapic_base_addr();

    serial_println!("Core {}: Initializing Local APIC...", core_id);

    // Enable the APIC by setting the Spurious Interrupt Vector Register
    // Bit 8 = APIC Software Enable/Disable
    // Bits 0-7 = Spurious vector
    let svr = lapic_ptr.offset(APICOffset::Svr as isize / 4);
    let current_svr = svr.read_volatile();
    svr.write_volatile(current_svr | (1 << 8) | 0xFF);
    serial_println!(
        "Core {}: APIC enabled (SVR = {:#x})",
        core_id,
        current_svr | (1 << 8) | 0xFF
    );

    // Mask all LVT entries initially
    lapic_ptr
        .offset(APICOffset::LvtT as isize / 4)
        .write_volatile(0x10000);
    lapic_ptr
        .offset(APICOffset::LvtLint0 as isize / 4)
        .write_volatile(0x10000);
    lapic_ptr
        .offset(APICOffset::LvtLint1 as isize / 4)
        .write_volatile(0x10000);
    lapic_ptr
        .offset(APICOffset::LvtErr as isize / 4)
        .write_volatile(0x10000);
    lapic_ptr
        .offset(APICOffset::LvtPmcr as isize / 4)
        .write_volatile(0x10000);
    lapic_ptr
        .offset(APICOffset::LvtTsr as isize / 4)
        .write_volatile(0x10000);
    serial_println!("Core {}: LVT entries masked", core_id);

    lapic_ptr
        .offset(APICOffset::Esr as isize / 4)
        .write_volatile(0);
    lapic_ptr
        .offset(APICOffset::Esr as isize / 4)
        .write_volatile(0);

    lapic_ptr
        .offset(APICOffset::Eoi as isize / 4)
        .write_volatile(0);

    // Set Task Priority to 0
    lapic_ptr
        .offset(APICOffset::Tpr as isize / 4)
        .write_volatile(0);

    // Set Logical Destination Register
    lapic_ptr
        .offset(APICOffset::Ldr as isize / 4)
        .write_volatile((core_id as u32) << 24);

    // Set Destination Format Register for flat model
    lapic_ptr
        .offset(APICOffset::Dfr as isize / 4)
        .write_volatile(0xFFFFFFFF);

    serial_println!("Core {}: APIC initialization complete", core_id);
}

unsafe fn init_local_apic(
    phys_address: u32,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    serial_println!("Mapping Local APIC");

    let virt_addr = unsafe { map_apic_mem(phys_address, LAPIC_VIRT_BASE, mapper, frame_allocator) };

    let local_apic_ptr = virt_addr.as_mut_ptr::<u32>();

    LAPIC_ADDRESS.store(local_apic_ptr, Ordering::Release);

    unsafe {
        // Set Task Priority Register to 0
        local_apic_ptr
            .offset(APICOffset::Tpr as isize / 4)
            .write_volatile(0);

        // Set Spurious Interrupt Vector Register
        let svr_offset = APICOffset::Svr as isize / 4;
        local_apic_ptr.offset(svr_offset).write_volatile(0x1FF);

        // Set Logical Destination
        local_apic_ptr
            .offset(APICOffset::Ldr as isize / 4)
            .write_volatile(0x01000000);
        local_apic_ptr
            .offset(APICOffset::Dfr as isize / 4)
            .write_volatile(0xFFFFFFFF);

        let lvt_lint0 = local_apic_ptr.offset(APICOffset::LvtLint0 as isize / 4);
        lvt_lint0.write_volatile(0x10000);

        let lvt_lint1 = local_apic_ptr.offset(APICOffset::LvtLint1 as isize / 4);
        lvt_lint1.write_volatile(0x10000);

        local_apic_ptr
            .offset(APICOffset::LvtT as isize / 4)
            .write_volatile(0x10000);
        local_apic_ptr
            .offset(APICOffset::LvtPmcr as isize / 4)
            .write_volatile(0x10000);
        local_apic_ptr
            .offset(APICOffset::LvtTsr as isize / 4)
            .write_volatile(0x10000);
        local_apic_ptr
            .offset(APICOffset::LvtErr as isize / 4)
            .write_volatile(0x10000);

        serial_println!("Local APIC initialized, all LVTs masked (using I/O APIC)");
    }
}

/// Read the Time Stamp Counter (TSC) register
/// Returns the current cycle count since processor reset
pub unsafe fn tsc_read() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "lfence",
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nostack, preserves_flags)
        );
    }
    ((high as u64) << 32) | (low as u64)
}

unsafe fn init_timer_periodic_mode(local_apic_ptr: *mut u32, core_id: u8) {
    let tsc_freq = TSC_FREQUENCY_HZ.load(Ordering::Relaxed);
    serial_println!("Core {}: init_timer_periodic_mode", core_id);
    serial_println!("TSC Frequency: {} Hz", tsc_freq);

    unsafe {
        // determine the APIC timer's bus frequency
        // configure APIC timer in one-shot mode to measure
        let lvt_timer = local_apic_ptr.offset(APICOffset::LvtT as isize / 4);
        // use one-shot mode with divider 1 for measurement
        let tdcr = local_apic_ptr.offset(APICOffset::Tdcr as isize / 4);
        tdcr.write_volatile(0x0);
        lvt_timer.write_volatile(InterruptIndex::Timer as u32);

        // set a large initial count
        let ticr = local_apic_ptr.offset(APICOffset::Ticr as isize / 4);
        let test_count = 1_000_000;
        ticr.write_volatile(test_count);

        // wait for timer to fire, poll the timer current count reg
        let tccr = local_apic_ptr.offset(APICOffset::Tccr as isize / 4);
        let start = tsc_read();
        while tccr.read_volatile() != 0 {}
        let end = tsc_read();
        let tsc_cycles = end - start;

        let apic_bus_freq = (test_count as u64 * tsc_freq) / tsc_cycles;

        // configure periodic mode
        let tick_freq = TIMER_TICK_FREQ_HZ;
        let divider = 16;
        let apic_timer_freq = apic_bus_freq / divider;
        let ticks_needed = (apic_timer_freq / tick_freq) as u32;

        let svr = local_apic_ptr.offset(APICOffset::Svr as isize / 4);
        let current_svr = svr.read_volatile();
        svr.write_volatile(current_svr | (1 << 8) | 0xFF);
        serial_println!("Core {}: apic enabled", core_id);

        // Set divider to 16
        tdcr.write_volatile(0x3);
        // set periodic mode
        const LVTT_TSC_PERIODIC_MODE: u32 = 1 << 17;
        lvt_timer.write_volatile(InterruptIndex::Timer as u32 | LVTT_TSC_PERIODIC_MODE);
        // set tick rate
        ticr.write_volatile(ticks_needed);

        serial_print!("Core {}: Timer configured in periodic mode:\n  APIC Bus frequency: {} Hz\n  APIC timer frequency: 
            {} Hz\n  Ticks per interrupt: {} (set tick frequency: {} Hz)", core_id, apic_bus_freq, apic_timer_freq, ticks_needed, tick_freq);
    }
}

pub unsafe fn set_tsc_aux(core_id: u8) {
    // Store core ID in low byte of TSC_AUX
    let aux_value = core_id as u64;

    // Write to IA32_TSC_AUX MSR (0xC0000103)
    let low = aux_value as u32;
    let high = (aux_value >> 32) as u32;

    asm!(
        "wrmsr",
        in("ecx") 0xC0000103_u32,
        in("eax") low,
        in("edx") high,
        options(nostack, preserves_flags)
    );
}

pub unsafe fn init_timer_for_core(core_id: u8) {
    if !TIMER_ENABLED {
        return;
    }

    serial_println_core!("Initializing timer for Core {}", core_id);

    let cpu_info = get_cpu_info_for_core(core_id);
    let lapic_addr = get_lapic_base_addr();

    if !cpu_info.features.contains(CpuFeatureFlags::TSC_DEADLINE) {
        serial_println_core!("TSC-Deadline mode not supported, falling back to periodic mode");
        unsafe { init_timer_periodic_mode(lapic_addr, core_id) };
        return;
    }

    unsafe { init_timer_tsc(lapic_addr, core_id) };
}

pub unsafe fn init_timer_tsc(local_apic_ptr: *mut u32, core_id: u8) {
    serial_println_core!("TSC-Deadline mode supported, using for timer");

    unsafe {
        let svr = local_apic_ptr.offset(APICOffset::Svr as isize / 4);
        let current_svr = svr.read_volatile();
        svr.write_volatile(current_svr | (1 << 8) | 0xFF);

        let lvt_timer = local_apic_ptr.offset(APICOffset::LvtT as isize / 4);
        const LVTT_TSC_DEADLINE_MODE: u32 = 1 << 18;
        lvt_timer.write_volatile(InterruptIndex::Timer as u32 | LVTT_TSC_DEADLINE_MODE);

        use core::arch::x86_64::_mm_mfence;
        _mm_mfence();

        let tsc_freq = TSC_FREQUENCY_HZ.load(Ordering::Relaxed);
        serial_println!("TSC Frequency: {} Hz", tsc_freq);
        let tsc_deadline = tsc_read() + tsc_cycles_per_tick();

        // write the deadline to the MSR to arm it
        msr_write(MSR_IA32_TSC_DEADLINE, tsc_deadline);
    }

    serial_println!("Timer configured in TSC-Deadline mode");
}

unsafe fn init_keyboard(local_apic_ptr: *mut u32) {
    // unsafe {
    //     let keyboard_register = local_apic_ptr.offset(APICOffset::LvtLint1 as isize / 4);
    //     keyboard_register.write_volatile(InterruptIndex::Keyboard as u8 as u32);
    // }
}

pub fn enable_interrupts() {
    serial_println_core!("Enabling interrupts");
    // Enable interrupts on the CPU
    x86_64::instructions::interrupts::enable();
    serial_println_core!("Interrupts enabled");
}

pub fn disable_interrupts() {
    x86_64::instructions::interrupts::disable();
}

// Store the virtual address of the mapped Local APIC
static LAPIC_VIRT_ADDR: spin::Mutex<Option<VirtAddr>> = spin::Mutex::new(None);

/// Map the Local APIC for the current core (must be called on each core)
pub unsafe fn map_local_apic_for_current_core(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> *mut u32 {
    // Read the physical address from MSR
    let lapic_phys = get_lapic_base_addr_phys();
    use x86_64::registers::control::Cr3;
    let (active_pml4_frame, _) = Cr3::read();
    serial_println!(
        "2 Active PML4 frame: {:#x}",
        active_pml4_frame.start_address().as_u64()
    );

    serial_println!("map_local_apic_for_current_core: {:#x}", lapic_phys);

    // Create a virtual address - use a fixed offset from the HHDM or map it directly
    // Option 1: Use HHDM offset if you have it (identity mapping with offset)
    // let hhdm_offset = crate::boot_info::boot_info().hhdm_offset;
    // let virt_addr = VirtAddr::new(lapic_phys + hhdm_offset);

    // Option 2: Map it explicitly (safer, works without HHDM)
    // We'll map it to a known virtual address range for APIC
    //let apic_virt_base = 0xFFFF_8000_0000_0000 + lapic_phys; // Example: high canonical address
    let apic_virt_base = LAPIC_VIRT_BASE + lapic_phys;
    let page = Page::containing_address(VirtAddr::new(apic_virt_base));
    let phys_frame = PhysFrame::containing_address(PhysAddr::new(lapic_phys));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;

    unsafe {
        mapper
            .map_to(page, phys_frame, flags, frame_allocator)
            .unwrap()
            .flush();
    }

    // Store the virtual address for later use
    *LAPIC_VIRT_ADDR.lock() = Some(page.start_address());

    page.start_address().as_mut_ptr::<u32>()
}

/// Get the physical address of the Local APIC from MSR
pub fn get_lapic_base_addr_phys() -> u64 {
    const IA32_APIC_BASE_MSR: u32 = 0x1B;
    unsafe {
        let low: u32;
        let high: u32;
        asm!(
            "rdmsr",
            in("ecx") IA32_APIC_BASE_MSR,
            out("eax") low,
            out("edx") high,
            options(nostack, preserves_flags)
        );
        let apic_base = ((high as u64) << 32) | (low as u64);
        apic_base & 0xFFFFF000
    }
}

/// Get the virtual address of the Local APIC (after mapping)
pub fn get_lapic_base_addr() -> *mut u32 {
    LAPIC_ADDRESS.load(Ordering::Acquire)
}

pub unsafe fn init_acpi(
    rsdp_addr: usize,
    phys_offset: u64,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    serial_println!("init_acpi()");
    let handler = IdentityAcpiHandler {
        phys_offset: phys_offset,
    };
    serial_println!("creating AcpiTables");
    let acpi_tables =
        unsafe { AcpiTables::from_rsdp(handler, rsdp_addr).expect("Failed to parse ACPI") };

    serial_println!("AcpiTables initialized");

    let acpi_platform: AcpiPlatform<IdentityAcpiHandler> =
        AcpiPlatform::new(acpi_tables, handler).expect("Cannot create AcpiPlatform");

    let mut lapic_addr: u32 = 0;
    let mut io_apic_addr: u32 = 0;
    let mut got_apic_addr = false;

    serial_println!("AcpiPlatform created");

    match acpi_platform.interrupt_model {
        InterruptModel::Apic(apic) => {
            serial_println!("APIC supported");
            lapic_addr = apic.local_apic_address as u32;
            serial_println!("Found {} IO APICs", apic.io_apics.len());
            let io_apic = apic.io_apics.get(0).unwrap();
            let io_apic_id = io_apic.id;
            io_apic_addr = io_apic.address;
            got_apic_addr = true;
            let gsi_base = io_apic.global_system_interrupt_base;

            serial_println!("LAPIC at addr: {:#x}", lapic_addr);

            serial_println!(
                "IOAPIC {} at addr: {:#x}, GSI base {}",
                io_apic_id,
                io_apic_addr,
                gsi_base
            );
            let ioapic_virt =
                unsafe { map_apic_mem_identity(io_apic_addr, mapper, frame_allocator) };
            let ioapic_ptr = ioapic_virt.as_mut_ptr::<u32>();

            let mut keyboard_irq: u32 = 1;
            #[cfg(feature = "ps2_mouse")]
            let mut mouse_irq: u32 = 12;

            for iso in apic.interrupt_source_overrides.iter() {
                match iso.isa_source {
                    1 => keyboard_irq = iso.global_system_interrupt,
                    #[cfg(feature = "ps2_mouse")]
                    12 => mouse_irq = iso.global_system_interrupt,
                    _ => {}
                }
                serial_println!(
                    "ISO: ISA IRQ {} -> GSI {}",
                    iso.isa_source,
                    iso.global_system_interrupt,
                );
            }

            unsafe {
                set_ioapic_redirection(
                    ioapic_ptr,
                    keyboard_irq,
                    InterruptIndex::Keyboard as u8,
                    false,
                );
                #[cfg(feature = "ps2_mouse")]
                set_ioapic_redirection(ioapic_ptr, mouse_irq, InterruptIndex::Mouse as u8, false);
            }
        }
        _ => {
            serial_println!("APIC not supported");
        }
    }
    unsafe {
        init_local_apic(lapic_addr, mapper, frame_allocator);
    }

    disable_pic();

    #[cfg(feature = "ps2_mouse")]
    {
        MOUSE.lock().init().unwrap();
        MOUSE.lock().set_on_complete(mouse_on_packed_processed);
        serial_println_core!("Mouse initialized");
    }
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // CPU exceptions without error codes
        idt.divide_error.set_handler_fn(divide_by_zero_handler);
        idt.debug.set_handler_fn(debug_handler);
        idt.non_maskable_interrupt.set_handler_fn(nmi_handler);
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.overflow.set_handler_fn(overflow_handler);
        idt.bound_range_exceeded.set_handler_fn(bound_range_exceeded_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.device_not_available.set_handler_fn(device_not_available_handler);
        idt.alignment_check.set_handler_fn(alignment_check_handler);

        // CPU exceptions with error codes
        idt.invalid_tss.set_handler_fn(invalid_tss_handler);
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_fault_handler);
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
        idt.page_fault.set_handler_fn(pagefault_handler);
        idt.simd_floating_point.set_handler_fn(simd_floating_point_handler);

        idt.security_exception.set_handler_fn(security_exception_handler);

        // Hardware interrupts
        unsafe {
            idt[InterruptIndex::Timer as u8]
                .set_handler_addr(VirtAddr::new(timer_interrupt_stub as usize as u64));
            idt[InterruptIndex::Keyboard as u8]
                .set_handler_addr(VirtAddr::new(keyboard_interrupt_stub as usize as u64));
            idt[InterruptIndex::Reschedule as u8]
                .set_handler_addr(VirtAddr::new(reschedule_interrupt_stub as usize as u64));
            #[cfg(feature = "ps2_mouse")]
            idt[InterruptIndex::Mouse as u8]
                .set_handler_addr(VirtAddr::new(mouse_interrupt_stub as usize as u64));
        }

        //unsafe {idt[0x80].set_handler_fn(syscall_int80_handler).set_stack_index(1)};

        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        idt
    };
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial_println_core!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn divide_by_zero_handler(stack_frame: InterruptStackFrame) {
    serial_println_core!("EXCEPTION: DIVIDE BY ZERO\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn debug_handler(stack_frame: InterruptStackFrame) {
    serial_println_core!("EXCEPTION: DEBUG\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn nmi_handler(stack_frame: InterruptStackFrame) {
    serial_println_core!("EXCEPTION: NON-MASKABLE INTERRUPT\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn overflow_handler(stack_frame: InterruptStackFrame) {
    serial_println_core!("EXCEPTION: OVERFLOW\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn bound_range_exceeded_handler(stack_frame: InterruptStackFrame) {
    serial_println_core!("EXCEPTION: BOUND RANGE EXCEEDED\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    serial_println_core!("EXCEPTION: INVALID OPCODE\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn device_not_available_handler(stack_frame: InterruptStackFrame) {
    serial_println_core!("EXCEPTION: DEVICE NOT AVAILABLE\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn alignment_check_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println_core!(
        "EXCEPTION: ALIGNMENT CHECK\nError Code: {}\n{:#?}",
        error_code,
        stack_frame
    );
    hlt_loop();
}

extern "x86-interrupt" fn invalid_tss_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    serial_println_core!(
        "EXCEPTION: INVALID TSS\nError Code: {}\n{:#?}",
        error_code,
        stack_frame
    );
    hlt_loop();
}

extern "x86-interrupt" fn segment_not_present_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println_core!(
        "EXCEPTION: SEGMENT NOT PRESENT\nError Code: {}\n{:#?}",
        error_code,
        stack_frame
    );
    hlt_loop();
}

extern "x86-interrupt" fn stack_segment_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println_core!(
        "EXCEPTION: STACK SEGMENT FAULT\nError Code: {}\n{:#?}",
        error_code,
        stack_frame
    );
    hlt_loop();
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println_core!(
        "EXCEPTION: GENERAL PROTECTION FAULT\nError Code: {}\n{:#?}",
        error_code,
        stack_frame
    );

    serial_println!("EXCEPTION: GENERAL PROTECTION FAULT");
    serial_println!("Error Code: {}", error_code);
    serial_println!("RIP: {:#x}", stack_frame.instruction_pointer);
    serial_println!("CS: {:?}", stack_frame.code_segment);
    serial_println!("RFLAGS: {:?}", stack_frame.cpu_flags);
    serial_println!("RSP: {:#x}", stack_frame.stack_pointer);
    serial_println!("SS: {:?}", stack_frame.stack_segment);

    unsafe {
        let rsp = stack_frame.stack_pointer.as_u64() as *const u64;
        serial_println!("Stack contents for iretq:");
        serial_println!("  RIP: {:#x}", *rsp);
        serial_println!("  CS: {:#x}", *rsp.add(1));
        serial_println!("  RFLAGS: {:#x}", *rsp.add(2));
        serial_println!("  RSP: {:#x}", *rsp.add(3));
        serial_println!("  SS: {:#x}", *rsp.add(4));
    }

    let rip = stack_frame.instruction_pointer.as_u64();
    serial_println!("Faulting instruction at: {:#x}", rip);

    unsafe {
        let instr_ptr = rip as *const u8;
        serial_println!(
            "Instruction bytes: {:02x} {:02x} {:02x} {:02x} {:02x}",
            *instr_ptr,
            *instr_ptr.add(1),
            *instr_ptr.add(2),
            *instr_ptr.add(3),
            *instr_ptr.add(4)
        );
    }

    hlt_loop();
}

extern "x86-interrupt" fn simd_floating_point_handler(stack_frame: InterruptStackFrame) {
    serial_println_core!("EXCEPTION: SIMD FLOATING POINT\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn security_exception_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println_core!(
        "EXCEPTION: SECURITY EXCEPTION\nError Code: {}\n{:#?}",
        error_code,
        stack_frame
    );
    hlt_loop();
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "C" fn timer_interrupt_handler(frame: &mut TrapFrame) {
    let core_id = get_current_core_id();
    if is_user_frame(frame) {
        crate::gdt::assert_rsp_in_task_stack(core_id);
    }

    if TIMER_DEBUG_PRINT {
        serial_println_core!("*");
    }

    unsafe {
        if core_id == 0 {
            SYSTEM_TICKS.fetch_add(1, Ordering::Relaxed);
        }

        let next_deadline = tsc_read() + tsc_cycles_per_tick();
        msr_write(MSR_IA32_TSC_DEADLINE, next_deadline);
    }

    crate::process::scheduler::on_timer_tick(core_id);

    unsafe {
        interrupt_over();
    }
}

extern "C" fn reschedule_interrupt_handler(_frame: &mut TrapFrame) {
    let core_id = get_current_core_id();
    crate::process::scheduler::mark_core_for_reschedule(core_id);

    unsafe {
        interrupt_over();
    }
}

interrupt_stub!(timer_interrupt_stub, timer_interrupt_handler);
interrupt_stub!(keyboard_interrupt_stub, keyboard_interrupt_handler);
interrupt_stub!(reschedule_interrupt_stub, reschedule_interrupt_handler);
#[cfg(feature = "ps2_mouse")]
interrupt_stub!(mouse_interrupt_stub, mouse_interrupt_handler);

extern "C" fn keyboard_interrupt_handler(_frame: &mut TrapFrame) {
    use pc_keyboard::{
        DecodedKey, HandleControl, KeyState as PcKeyState, Keyboard, ScancodeSet1, layouts,
    };
    use spin::Mutex;
    use x86_64::instructions::port::Port;

    lazy_static! {
        static ref KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> =
            Mutex::new(Keyboard::new(
                ScancodeSet1::new(),
                layouts::Us104Key,
                HandleControl::Ignore
            ));
    }
    if KEYBOARD_DEBUG_PRINT {
        serial_println_core!("Keyboard interrupt received");
    }

    let mut keyboard = KEYBOARD.lock();
    let mut keyboard_port = Port::new(0x60);
    // SAFETY: This port is only read from in this interrupt handler.
    let scancode: u8 = unsafe { keyboard_port.read() };

    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        let key_state = match key_event.state {
            PcKeyState::Down | PcKeyState::SingleShot => KeyState::Pressed,
            PcKeyState::Up => KeyState::Released,
        };

        let key_code = key_event.code;
        let decoded_key = keyboard.process_keyevent(key_event);
        let modifiers = KeyModifiers::from(keyboard.get_modifiers());

        // decoding returns None for every key release, so key state is reported separately.
        // Pushed first so hotkeys see their modifier as held
        let _ = compositor::get_input_event_buffer().push(InputEvent::new_key_state(
            key_code as u32,
            key_state,
            modifiers,
        ));

        if let Some(decoded_key) = decoded_key {
            match decoded_key {
                DecodedKey::Unicode(c) => {
                    unsafe {
                        compositor::get_input_event_buffer()
                            .push(InputEvent::new_char(c as u32, key_state, modifiers));
                    };
                    if KEYBOARD_DEBUG_PRINT {
                        serial_println_core!("keyboard_handler: Pushed char: {}", c);
                    }
                }
                DecodedKey::RawKey(k) => {
                    unsafe {
                        compositor::get_input_event_buffer()
                            .push(InputEvent::new_key(k as u32, key_state, modifiers));
                    };
                    if KEYBOARD_DEBUG_PRINT {
                        serial_println_core!("keyboard_handler: Pushed key: {:?}", k);
                    }
                }
            };
        }
    }

    unsafe {
        interrupt_over();
    }
}

#[cfg(feature = "ps2_mouse")]
extern "C" fn mouse_interrupt_handler(_frame: &mut TrapFrame) {
    if MOUSE_DEBUG_PRINT {
        serial_println_core!("Mouse interrupt received");
    }

    let mut mouse_port = Port::new(0x60);
    let packet: u8 = unsafe { mouse_port.read() };
    let mut mouse = MOUSE.lock();
    mouse.process_packet(packet);

    unsafe {
        interrupt_over();
    }
}

#[cfg(feature = "ps2_mouse")]
fn mouse_on_packed_processed(mouse_state: MouseState) {
    use crate::events::event_buffer::{MouseButtons, MouseEvent};
    if MOUSE_DEBUG_PRINT {
        serial_println_core!("mouse_on_packed_processed: Mouse state: {:?}", mouse_state);
    }

    let buttons = {
        let mut mouse_buttons = MouseButtons::empty();
        if mouse_state.left_button_down() {
            mouse_buttons |= MouseButtons::LEFT;
        }
        if mouse_state.right_button_down() {
            mouse_buttons |= MouseButtons::RIGHT;
        }
        // Middle button not accessible via public API
        //TODO: can try to read from mem layout
        mouse_buttons
    };

    // Overflow flags not accessible via public API
    let x_overflow = false;
    let y_overflow = false;

    let mouse_event = MouseEvent {
        x_delta: mouse_state.get_x(),
        y_delta: -mouse_state.get_y(),
        buttons,
        x_overflow,
        y_overflow,
    };

    let input_event = InputEvent::new_mouse(mouse_event);
    match unsafe { compositor::get_input_event_buffer().push(input_event) } {
        Ok(()) => {
            if MOUSE_DEBUG_PRINT {
                serial_println_core!("Mouse event pushed to buffer: {:?}", mouse_event);
            }
        }
        Err(e) => {
            serial_println_core!("Failed to push mouse event to buffer: {:?}", e);
        }
    }
}

extern "x86-interrupt" fn pagefault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    serial_println_core!("EXCEPTION: PAGE FAULT");
    serial_println_core!("Accessed Address: {:?}", Cr2::read());
    serial_println_core!("Error Code: {:?}", error_code);
    serial_println_core!("{:#?}", stack_frame);

    if error_code.contains(PageFaultErrorCode::USER_MODE) {
        serial_println_core!(
            "REMINDER: User stack is {} bytes wide",
            DEFAULT_NEW_PROCESS_STACK_SIZE
        );

        // Check if the faulting address is near the user stack limit
        let accessed_addr = Cr2::read().unwrap_or(VirtAddr::zero());
        let stack_pointer = stack_frame.stack_pointer;

        if stack_pointer.as_u64() > accessed_addr.as_u64() {
            let stack_usage = stack_pointer.as_u64() - accessed_addr.as_u64();
            if stack_usage > DEFAULT_NEW_PROCESS_STACK_SIZE {
                serial_println_core!(
                    "WARNING: Stack pointer exceeds {} limit! Possible stack overflow.",
                    DEFAULT_NEW_PROCESS_STACK_SIZE
                );
            }
        }
    }

    hlt_loop();
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = 32,
    Keyboard,
    Mouse = 44,
    /// Another core asks this one to look at its run queue
    Reschedule = 0xF0,
}
