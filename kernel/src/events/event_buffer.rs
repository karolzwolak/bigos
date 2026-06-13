use core::sync::atomic::{AtomicU32, Ordering};

use crate::{memory::paging::PAGE_SIZE, serial_println};
use spin::Mutex;

pub static EVENT_BUFFER: EventBuffer = EventBuffer::new();

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EventType {
    None = 0,
    KeyEvent = 1,
    MouseEvent = 2,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct InputEvent {
    pub event_type: EventType,
    pub _pad: [u8; 3],
    pub value: u32,
    pub extra: u32,
    pub reserved: u32,
}

impl InputEvent {
    pub const fn new(event_type: EventType, value: u32, extra: u32) -> Self {
        Self {
            event_type,
            _pad: [0; 3],
            value,
            extra,
            reserved: 0,
        }
    }

    pub fn new_key(keycode: u32, state: KeyState) -> Self {
        Self::new(EventType::KeyEvent, keycode, state as u32)
    }
}

const BUFFER_HEADER_SIZE: usize = core::mem::size_of::<AtomicU32>() * 3;
const MAX_EVENT_COUNT: usize =
    (PAGE_SIZE - BUFFER_HEADER_SIZE) / core::mem::size_of::<InputEvent>();

#[repr(C, align(4096))]
pub struct EventBuffer {
    pub write_idx: AtomicU32,
    pub read_idx: AtomicU32,
    pub event_count: AtomicU32,

    pub events: [InputEvent; MAX_EVENT_COUNT],
}

impl EventBuffer {
    pub const fn new() -> Self {
        Self {
            write_idx: AtomicU32::new(0),
            read_idx: AtomicU32::new(0),
            event_count: AtomicU32::new(0),
            events: [InputEvent::new(EventType::None, 0, 0); MAX_EVENT_COUNT],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.event_count.load(Ordering::Acquire) == 0
    }

    /// We assume only one event source can push at one time
    pub fn push(&self, event: InputEvent) -> Result<(), ()> {
        let event_count = self.event_count.load(Ordering::Acquire);
        if event_count >= MAX_EVENT_COUNT as u32 {
            serial_println!("Event buffer is full, dropping event: {:?}", event);
            return Err(());
        }

        let idx = self.write_idx.load(Ordering::Acquire);
        let event_ptr: *mut InputEvent = core::ptr::addr_of!(self.events[idx as usize]) as *mut InputEvent;
        unsafe {
            core::ptr::write_volatile(event_ptr, event);
        }

        core::sync::atomic::fence(Ordering::Release);

        self.write_idx.store((idx + 1) % MAX_EVENT_COUNT as u32, Ordering::Release);
        self.event_count.fetch_add(1, Ordering::Release);

        Ok(())
    }

    pub fn read(&self) -> Option<InputEvent> {
        if self.is_empty() {
            return None;
        }

        let idx = self.read_idx.load(Ordering::Acquire);
        let event = self.events[idx as usize];

        core::sync::atomic::fence(Ordering::Acquire);

        self.read_idx.store((idx + 1) % MAX_EVENT_COUNT as u32, Ordering::Release);
        self.event_count.fetch_sub(1, Ordering::Release);

        Some(event)
    }
}