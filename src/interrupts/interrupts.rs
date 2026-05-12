use crate::{gdt, hlt_loop, vga_print, vga_println};
use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

const TIMER_DEBUG_PRINT: bool = false;
const KEYBOARD_DEBUG_PRINT: bool = false;
const KEYBOARD_PORT: u16 = 0x60;

/// Set the first PIC offset to 32 to avoid overlap with the 32 exception slots
const PIC_1_OFFSET: u8 = 32;
const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;
// SAFETY: We are the only ones initializing the PICs here.
static PICS: spin::Mutex<ChainedPics> =
    spin::Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.page_fault.set_handler_fn(pagefault_handler);
        idt[InterruptIndex::Timer as u8].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard as u8].set_handler_fn(keyboard_interrupt_handler);

        // SAFETY: The index is valid and only used once.
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        idt
    };
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
}

pub fn init_idt() {
    IDT.load();
}

pub fn init_hw_interrupts() {
    // SAFETY: We are the only ones initializing the PICs here.
    unsafe {
        PICS.lock().initialize();
    }
    x86_64::instructions::interrupts::enable();
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    vga_println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    if TIMER_DEBUG_PRINT {
        vga_print!("*")
    };
    // SAFETY: We are the only ones notifying the PICs here.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer as u8);
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use pc_keyboard::{DecodedKey, HandleControl, Keyboard, ScancodeSet1, layouts};
    use spin::Mutex;
    use x86_64::instructions::port::Port;

    lazy_static! {
        static ref KEYBOARD: Mutex<Keyboard<layouts::Uk105Key, ScancodeSet1>> =
            Mutex::new(Keyboard::new(
                ScancodeSet1::new(),
                layouts::Uk105Key,
                HandleControl::Ignore
            ));
    }
    let mut keyboard = KEYBOARD.lock();
    let mut keyboard_port = Port::new(KEYBOARD_PORT);
    // SAFETY: This port is only read from in this interrupt handler.
    let scancode: u8 = unsafe { keyboard_port.read() };

    if let Ok(Some(event)) = keyboard.add_byte(scancode)
        && let Some(decoded_key) = keyboard.process_keyevent(event)
        && KEYBOARD_DEBUG_PRINT
    {
        match decoded_key {
            DecodedKey::Unicode(character) => vga_print!("{}", character),
            DecodedKey::RawKey(key) => vga_print!("{:?}", key),
        }
    }

    // SAFETY: We are the only ones notifying the PICs here.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard as u8);
    }
}

extern "x86-interrupt" fn pagefault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    vga_println!("EXCEPTION: PAGE FAULT");
    vga_println!("Accessed Address: {:?}", Cr2::read());
    vga_println!("Error Code: {:?}", error_code);
    vga_println!("{:#?}", stack_frame);
    hlt_loop();
}

#[cfg(test)]
mod tests {
    use x86_64::instructions::interrupts;

    #[test_case]
    fn breakpoint_exception() {
        interrupts::int3();
    }
}
