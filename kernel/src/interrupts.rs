#![allow(unused)]
use crate::process::syscall::init_syscall;
use crate::process::{
    process_manager::{ARCHE_PID, PROCESS_MANAGER},
    task::INVALID_PID,
};
use crate::util::msr::{msr_read, msr_write};
use crate::{
    events::event_buffer::{EVENT_BUFFER, InputEvent, KeyState, Keys},
    gdt, hlt_loop,
    memory::paging::{IdendtityAcpiHandler, MemoryMapFrameAllocator},
    serial_print, serial_println,
    util::cpuinfo::{CpuFeatureFlags, get_cpu_info},
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
use lazy_static::lazy_static;
use pc_keyboard::KeyCode;
use spin::Mutex;
use x86_64::{
    PhysAddr, VirtAddr,
    registers::rflags::RFlags,
    structures::{
        idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode},
        paging::{FrameAllocator, Mapper, Page, PageTableFlags, PhysFrame, Size4KiB},
    },
};

const TIMER_DEBUG_PRINT: bool = false;
const KEYBOARD_DEBUG_PRINT: bool = false;
const TIMER_ENABLED: bool = false;

pub const TSC_MOCK_FREQUENCY: u64 = 2400000000u64;
pub const TIMER_TICK_INTERVAL_MS: u64 = 100;
pub const TIMER_TICK_FREQ_DIVIDER: u64 = TIMER_TICK_INTERVAL_MS * 1000;
const MSR_IA32_TSC_DEADLINE: u32 = 0x6E0;

pub struct LapicPtr {
    address: *mut u32,
}
// SAFETY: The pointer is only used for memory-mapped I/O registers which are
// safe to access from multiple threads by design
unsafe impl Send for LapicPtr {}
unsafe impl Sync for LapicPtr {}

lazy_static! {
    pub static ref LAPIC_ADDRESS: Mutex<LapicPtr> = Mutex::new(LapicPtr {
        address: core::ptr::null_mut()
    });
}

pub fn init_idt() {
    serial_println!("init_idt");
    IDT.load();
}

fn disable_pic() {
    use x86_64::instructions::port::Port;
    unsafe {
        Port::<u8>::new(0xA1).write(0xFF);
    }
}

/// # Safety
///
/// Must only be called from within an interrupt handler, after the interrupt has been fully
/// processed. The LAPIC must have been initialized and `LAPIC_ADDRESS` must hold a valid
/// memory-mapped pointer to the local APIC registers.
pub unsafe fn interrupt_over() {
    unsafe {
        let local_apic_ptr = LAPIC_ADDRESS.lock().address;
        local_apic_ptr
            .offset(APICOffset::Eoi as isize / 4)
            .write_volatile(0);
    }
}

unsafe fn map_apic_mem(
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

unsafe fn init_io_apic(
    phys_address: u32,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    serial_println!("Mapping IO APIC");

    let virt_addr = unsafe { map_apic_mem(phys_address, mapper, frame_allocator) };

    let io_apic_ptr = virt_addr.as_mut_ptr::<u32>();

    unsafe {
        io_apic_ptr.offset(0).write_volatile(0x12);
        io_apic_ptr
            .offset(4)
            .write_volatile(InterruptIndex::Keyboard as u8 as u32);
    }
}

unsafe fn init_local_apic(
    phys_address: u32,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    serial_println!("Mapping Local APIC");

    let virt_addr = unsafe { map_apic_mem(phys_address, mapper, frame_allocator) };

    let local_apic_ptr = virt_addr.as_mut_ptr::<u32>();

    LAPIC_ADDRESS.lock().address = local_apic_ptr;

    unsafe {
        init_timer(local_apic_ptr);
        init_keyboard(local_apic_ptr);
    }
}

/// Read the Time Stamp Counter (TSC) register
/// Returns the current cycle count since processor reset
unsafe fn tsc_read() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nostack, preserves_flags)
        );
    }
    ((high as u64) << 32) | (low as u64)
}

unsafe fn init_timer_periodic_mode(local_apic_ptr: *mut u32) {
    let tsc_freq = TSC_MOCK_FREQUENCY;
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
        let tick_freq = TIMER_TICK_FREQ_DIVIDER;
        let divider = 16;
        let apic_timer_freq = apic_bus_freq / divider;
        let ticks_needed = (apic_timer_freq / tick_freq) as u32;

        let svr = local_apic_ptr.offset(APICOffset::Svr as isize / 4);
        let current_svr = svr.read_volatile();
        svr.write_volatile(current_svr | (1 << 8) | 0xFF);

        // Set divider to 16
        tdcr.write_volatile(0x3);
        // set periodic mode
        const LVTT_TSC_PERIODIC_MODE: u32 = 1 << 17;
        lvt_timer.write_volatile(InterruptIndex::Timer as u32 | LVTT_TSC_PERIODIC_MODE);
        // set tick rate
        ticr.write_volatile(ticks_needed);

        serial_println!("Timer configured in periodic mode:");
        serial_println!("  APIC Bus frequency: {} Hz", apic_bus_freq);
        serial_println!("  APIC timer frequency: {} Hz", apic_timer_freq);
        serial_println!(
            "  Ticks per interrupt: {} (set tick frequency: {} Hz)",
            ticks_needed,
            tick_freq
        );
    }
}

/// # Safety
///
/// `local_apic_ptr` must be a valid pointer to the memory-mapped LAPIC register file. The LAPIC
/// must have been previously mapped and enabled. This function must not be called concurrently.
pub unsafe fn init_timer(local_apic_ptr: *mut u32) {
    if !TIMER_ENABLED {
        return;
    }

    if !get_cpu_info()
        .features
        .contains(CpuFeatureFlags::TSC_DEADLINE)
    {
        serial_println!("TSC-Deadline mode not supported, falling back to periodic mode");
        unsafe { init_timer_periodic_mode(local_apic_ptr) };
        return;
    }

    serial_println!("TSC-Deadline mode supported, using for timer");

    unsafe {
        let svr = local_apic_ptr.offset(APICOffset::Svr as isize / 4);
        let current_svr = svr.read_volatile();
        svr.write_volatile(current_svr | (1 << 8) | 0xFF);

        let lvt_timer = local_apic_ptr.offset(APICOffset::LvtT as isize / 4);
        const LVTT_TSC_DEADLINE_MODE: u32 = 1 << 18;
        lvt_timer.write_volatile(InterruptIndex::Timer as u32 | LVTT_TSC_DEADLINE_MODE);

        use core::arch::x86_64::_mm_mfence;
        _mm_mfence();

        let tsc_freq = TSC_MOCK_FREQUENCY;
        serial_println!("TSC Frequency: {}", tsc_freq);
        let ticks_per_ms = tsc_freq / TIMER_TICK_FREQ_DIVIDER;
        let tsc_deadline = tsc_read() + ticks_per_ms;

        // write the deadline to the MSR to arm it
        msr_write(MSR_IA32_TSC_DEADLINE, tsc_deadline);
    }

    serial_println!("Timer configured in TSC-Deadline mode");
}

unsafe fn init_keyboard(local_apic_ptr: *mut u32) {
    unsafe {
        let keyboard_register = local_apic_ptr.offset(APICOffset::LvtLint1 as isize / 4);
        keyboard_register.write_volatile(InterruptIndex::Keyboard as u8 as u32);
    }
}

pub fn enable_interrupts() {
    serial_println!("Enabling interrupts");
    // Enable interrupts on the CPU
    x86_64::instructions::interrupts::enable();
    serial_println!("Interrupts enabled");
}

pub fn disable_interrupts() {
    x86_64::instructions::interrupts::disable();
}

/// # Safety
///
/// `rsdp_addr` must be a valid physical address of the RSDP structure as provided by the
/// bootloader. `phys_offset` must correctly map physical addresses to virtual addresses, and
/// `mapper` / `frame_allocator` must be valid for the duration of the call.
pub unsafe fn init_acpi(
    rsdp_addr: usize,
    phys_offset: u64,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    serial_println!("init_acpi()");
    let handler = IdendtityAcpiHandler { phys_offset };
    serial_println!("creating AcpiTables");
    let acpi_tables =
        unsafe { AcpiTables::from_rsdp(handler, rsdp_addr).expect("Failed to parse ACPI") };

    serial_println!("AcpiTables initialized");

    let acpi_platform: AcpiPlatform<IdendtityAcpiHandler> =
        AcpiPlatform::new(acpi_tables, handler).expect("Cannot create AcpiPlatform");

    let mut lapic_addr: u32 = 0;
    let mut io_apic_addr: u32 = 0;
    let mut got_apic_addr = false;

    match acpi_platform.interrupt_model {
        InterruptModel::Apic(apic) => {
            serial_println!("APIC supported");
            lapic_addr = apic.local_apic_address as u32;
            serial_println!("Found {} IO APICs", apic.io_apics.len());
            let io_apic = apic.io_apics.first().unwrap();
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
            /*
               local_apic_nmi_lines: Vec<NmiLine, A>,
               pub interrupt_source_overrides: Vec<InterruptSourceOverride, A>,
               pub nmi_sources: Vec<NmiSource, A>,
               pub also_has_legacy_pics: bool,
            */
        }
        _ => {
            serial_println!("APIC not supported");
        }
    }

    // let binding = acpi_platform
    //     .tables
    //     .find_table::<Madt>()
    //     .expect("Cannot find MADT table");
    // let madt_table = binding.get();

    // let local_apic_addr = madt_table.local_apic_address;
    // let flags = madt_table.flags;
    // serial_println!(
    //     "Found MADT table: local apic address: {:#x}, flags: {}",
    //     local_apic_addr,
    //     flags
    // );

    // for entry in madt_table.entries() {
    //     match entry {
    //         MadtEntry::LocalApic(local) => {
    //             let apic_id = local.apic_id;
    //             let processor_id = local.processor_id;

    //             serial_println!("Local APIC ID {} for CPU {}", apic_id, processor_id);
    //         }

    //         MadtEntry::IoApic(io_apic) => {
    //             io_apic_addr = io_apic.io_apic_address;
    //             got_io_apic_addr = true;
    //             let gsi_base = io_apic.global_system_interrupt_base;

    //             serial_println!(
    //                 "IOAPIC {} at addr: {:x}, GSI base {}",
    //                 io_apic.io_apic_id,
    //                 io_apic_addr,
    //                 gsi_base
    //             );
    //         }

    //         MadtEntry::InterruptSourceOverride(iso) => {
    //             let irq = iso.irq;
    //             let bus = iso.bus;
    //             let global_system_interrupt = iso.global_system_interrupt;
    //             serial_println!(
    //                 "IRQ {} on bus {} overridden to GSI {}",
    //                 irq,
    //                 bus,
    //                 global_system_interrupt
    //             );
    //         }

    //         MadtEntry::PlatformInterruptSource(e) => {
    //             // handle if needed
    //         }

    //         MadtEntry::NmiSource(nmi) => { /* handle NMI */ }

    //         _ => {}
    //     }
    // }

    unsafe {
        init_local_apic(lapic_addr, mapper, frame_allocator);
    }

    if got_apic_addr {
        unsafe {
            init_io_apic(io_apic_addr, mapper, frame_allocator);
        }
    } else {
        serial_println!("ERROR: Cannot find IO apic");
    }

    init_syscall();

    disable_pic();
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

        // Hardware interrupts
        idt[InterruptIndex::Timer as u8].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard as u8].set_handler_fn(keyboard_interrupt_handler);

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
    serial_println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn divide_by_zero_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: DIVIDE BY ZERO\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn debug_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: DEBUG\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn nmi_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: NON-MASKABLE INTERRUPT\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn overflow_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: OVERFLOW\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn bound_range_exceeded_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: BOUND RANGE EXCEEDED\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: INVALID OPCODE\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn device_not_available_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: DEVICE NOT AVAILABLE\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn alignment_check_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println!(
        "EXCEPTION: ALIGNMENT CHECK\nError Code: {}\n{:#?}",
        error_code,
        stack_frame
    );
    hlt_loop();
}

extern "x86-interrupt" fn invalid_tss_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    serial_println!(
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
    serial_println!(
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
    serial_println!(
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
    serial_println!(
        "EXCEPTION: GENERAL PROTECTION FAULT\nError Code: {}\n{:#?}",
        error_code,
        stack_frame
    );
    hlt_loop();
}

extern "x86-interrupt" fn simd_floating_point_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: SIMD FLOATING POINT\n{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    if TIMER_DEBUG_PRINT {
        serial_println!("*");
    };

    unsafe {
        // re-arm the timer for the next tick
        let next_deadline = tsc_read() + (TSC_MOCK_FREQUENCY / TIMER_TICK_FREQ_DIVIDER);
        msr_write(MSR_IA32_TSC_DEADLINE, next_deadline);

        interrupt_over();
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
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
    let mut keyboard = KEYBOARD.lock();
    let mut keyboard_port = Port::new(0x60);
    // SAFETY: This port is only read from in this interrupt handler.
    let scancode: u8 = unsafe { keyboard_port.read() };

    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        let is_press = key_event.state == PcKeyState::Down || key_event.state == PcKeyState::SingleShot;
        if let Some(decoded_key) = keyboard.process_keyevent(key_event) {
            if is_press {
                let value = match decoded_key {
                    DecodedKey::Unicode(c) => Some(c as u32),
                    DecodedKey::RawKey(KeyCode::ArrowUp) => Some(Keys::ArrowUp as u32),
                    _ => None,
                };
                if let Some(v) = value {
                    let _ = EVENT_BUFFER.push(InputEvent::new_key(v, KeyState::Pressed));
                }

                if KEYBOARD_DEBUG_PRINT {
                    match decoded_key {
                        DecodedKey::Unicode(c) => serial_print!("{}", c),
                        DecodedKey::RawKey(k) => serial_print!("{:?}", k),
                    }
                }
            }
        }
    }

    unsafe {
        interrupt_over();
    }
}

extern "x86-interrupt" fn pagefault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    serial_println!("EXCEPTION: PAGE FAULT");
    serial_println!("Accessed Address: {:?}", Cr2::read());
    serial_println!("Error Code: {:?}", error_code);
    serial_println!("{:#?}", stack_frame);
    hlt_loop();
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = 32,
    Keyboard,
}

#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy)]
#[repr(isize)]
#[allow(dead_code)]
pub enum APICOffset {
    R0x00 = 0x0,      // --reserved--
    R0x10 = 0x10,     // --reserved--
    Ir = 0x20,        // ID Register
    Vr = 0x30,        // Version Register
    R0x40 = 0x40,     // --reserved--
    R0x50 = 0x50,     // --reserved--
    R0x60 = 0x60,     // --reserved--
    R0x70 = 0x70,     // --reserved--
    Tpr = 0x80,       // Text Priority Register
    Apr = 0x90,       // Arbitration Priority Register
    Ppr = 0xA0,       // Processor Priority Register
    Eoi = 0xB0,       // End of Interrupt
    Rrd = 0xC0,       // Remote Read Register
    Ldr = 0xD0,       // Logical Destination Register
    Dfr = 0xE0,       // DFR
    Svr = 0xF0,       // Spurious (Interrupt) Vector Register
    Isr1 = 0x100,     // In-Service Register 1
    Isr2 = 0x110,     // In-Service Register 2
    Isr3 = 0x120,     // In-Service Register 3
    Isr4 = 0x130,     // In-Service Register 4
    Isr5 = 0x140,     // In-Service Register 5
    Isr6 = 0x150,     // In-Service Register 6
    Isr7 = 0x160,     // In-Service Register 7
    Isr8 = 0x170,     // In-Service Register 8
    Tmr1 = 0x180,     // Trigger Mode Register 1
    Tmr2 = 0x190,     // Trigger Mode Register 2
    Tmr3 = 0x1A0,     // Trigger Mode Register 3
    Tmr4 = 0x1B0,     // Trigger Mode Register 4
    Tmr5 = 0x1C0,     // Trigger Mode Register 5
    Tmr6 = 0x1D0,     // Trigger Mode Register 6
    Tmr7 = 0x1E0,     // Trigger Mode Register 7
    Tmr8 = 0x1F0,     // Trigger Mode Register 8
    Irr1 = 0x200,     // Interrupt Request Register 1
    Irr2 = 0x210,     // Interrupt Request Register 2
    Irr3 = 0x220,     // Interrupt Request Register 3
    Irr4 = 0x230,     // Interrupt Request Register 4
    Irr5 = 0x240,     // Interrupt Request Register 5
    Irr6 = 0x250,     // Interrupt Request Register 6
    Irr7 = 0x260,     // Interrupt Request Register 7
    Irr8 = 0x270,     // Interrupt Request Register 8
    Esr = 0x280,      // Error Status Register
    R0x290 = 0x290,   // --reserved--
    R0x2A0 = 0x2A0,   // --reserved--
    R0x2B0 = 0x2B0,   // --reserved--
    R0x2C0 = 0x2C0,   // --reserved--
    R0x2D0 = 0x2D0,   // --reserved--
    R0x2E0 = 0x2E0,   // --reserved--
    LvtCmci = 0x2F0,  // LVT Corrected Machine Check Interrupt (CMCI) Register
    Icr1 = 0x300,     // Interrupt Command Register 1
    Icr2 = 0x310,     // Interrupt Command Register 2
    LvtT = 0x320,     // LVT Timer Register
    LvtTsr = 0x330,   // LVT Thermal Sensor Register
    LvtPmcr = 0x340,  // LVT Performance Monitoring Counters Register
    LvtLint0 = 0x350, // LVT LINT0 Register
    LvtLint1 = 0x360, // LVT LINT1 Register
    LvtE = 0x370,     // LVT Error Register
    Ticr = 0x380,     // Initial Count Register (for Timer)
    Tccr = 0x390,     // Current Count Register (for Timer)
    R0x3A0 = 0x3A0,   // --reserved--
    R0x3B0 = 0x3B0,   // --reserved--
    R0x3C0 = 0x3C0,   // --reserved--
    R0x3D0 = 0x3D0,   // --reserved--
    Tdcr = 0x3E0,     // Divide Configuration Register (for Timer)
    R0x3F0 = 0x3F0,   // --reserved--
}
