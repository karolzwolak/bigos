use core::arch::x86_64::_rdtsc;
use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int};
use core::sync::atomic::{AtomicBool, Ordering};

use rustspace::{sys_present_window, sys_sleep_ms, EventReader, EventType, InputEvent, KeyState};

use crate::keys::{doom_key_for_keycodeval, DOOM_KEY_UNMAPPED};

const MILLISECONDS_PER_SECOND: u128 = 1000;
pub const DOOM_STRAFE_KEY: c_int = b'v' as c_int;
pub const DOOM_FIRE_KEY: c_int = b'c' as c_int;
pub const DOOM_SCREEN_WIDTH: u32 = 640;
pub const DOOM_SCREEN_HEIGHT: u32 = 400;
const KEY_EVENT_PRESSED: c_int = 1;
const KEY_EVENT_RELEASED: c_int = 0;
const DG_NO_KEY: c_int = 0;
const DG_KEY_AVAILABLE: c_int = 1;

unsafe extern "C" {
    static mut DG_ScreenBuffer: *mut u32;
    static mut key_strafe: c_int;
    static mut key_fire: c_int;
    fn free(pointer: *mut u8);
}

pub fn read_tsc() -> u64 {
    // SAFETY: rdtsc has no preconditions
    unsafe { _rdtsc() }
}

pub struct PlatformState {
    pub window_id: u32,
    pub event_buffer_addr: usize,
    pub back_buffer: *mut u32,
    pub front_buffer: *mut u32,
    pub tsc_frequency_hz: u64,
    pub start_tsc: u64,
}

impl PlatformState {
    pub const PLACEHOLDER: Self = Self {
        window_id: 0,
        event_buffer_addr: 0,
        back_buffer: core::ptr::null_mut(),
        front_buffer: core::ptr::null_mut(),
        tsc_frequency_hz: 1,
        start_tsc: 0,
    };
}

pub struct PlatformCell {
    state: UnsafeCell<PlatformState>,
    is_filled: AtomicBool,
}

// SAFETY: the doom process runs one thread, the state is never touched from two at once
unsafe impl Sync for PlatformCell {}

impl PlatformCell {
    /// # Safety
    ///
    /// No other reference to the state may be live
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn get(&self) -> &mut PlatformState {
        unsafe { &mut *self.state.get() }
    }

    pub fn fill(&self, state: PlatformState) {
        debug_assert!(!self.is_filled.load(Ordering::Acquire));

        // SAFETY: main fills the state once, before doomgeneric_Create
        unsafe { *self.get() = state };
        self.is_filled.store(true, Ordering::Release);
    }

    pub fn is_filled(&self) -> bool {
        self.is_filled.load(Ordering::Acquire)
    }
}

pub static PLATFORM: PlatformCell = PlatformCell {
    state: UnsafeCell::new(PlatformState::PLACEHOLDER),
    is_filled: AtomicBool::new(false),
};

#[unsafe(no_mangle)]
pub extern "C" fn DG_Init() {
    debug_assert!(PLATFORM.is_filled());

    // SAFETY: single threaded, called by doomgeneric_Create after main filled the state
    unsafe {
        let platform = PLATFORM.get();

        // replace the doomgeneric screen buffer with the window back buffer
        free(DG_ScreenBuffer as *mut u8);
        DG_ScreenBuffer = platform.back_buffer;
        key_strafe = DOOM_STRAFE_KEY;
        key_fire = DOOM_FIRE_KEY;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_DrawFrame() {
    // SAFETY: single threaded
    let platform = unsafe { PLATFORM.get() };

    sys_present_window(platform.window_id);
    core::mem::swap(&mut platform.back_buffer, &mut platform.front_buffer);

    // SAFETY: single threaded
    unsafe { DG_ScreenBuffer = platform.back_buffer };
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_SleepMs(duration_ms: u32) {
    sys_sleep_ms(duration_ms as u64);
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_GetTicksMs() -> u32 {
    // SAFETY: single threaded
    let platform = unsafe { PLATFORM.get() };
    debug_assert!(platform.tsc_frequency_hz != 0);

    let elapsed_cycles = read_tsc().wrapping_sub(platform.start_tsc);

    (elapsed_cycles as u128 * MILLISECONDS_PER_SECOND / platform.tsc_frequency_hz as u128) as u32
}

/// # Safety
///
/// `pressed` and `key` must be valid for writes, doom passes pointers to its locals
#[unsafe(no_mangle)]
pub unsafe extern "C" fn DG_GetKey(pressed: *mut c_int, key: *mut u8) -> c_int {
    // SAFETY: single threaded
    let platform = unsafe { PLATFORM.get() };
    debug_assert!(platform.event_buffer_addr != 0);

    // SAFETY: main stored the address the kernel mapped for this window
    let mut reader = unsafe { EventReader::new(platform.event_buffer_addr) };

    while let Some(event) = reader.try_read(platform.window_id) {
        let Some((is_pressed, doom_key)) = decode_key_event(&event) else {
            continue;
        };

        // SAFETY: see the function safety contract
        unsafe {
            *pressed = is_pressed;
            *key = doom_key;
        }

        return DG_KEY_AVAILABLE;
    }

    DG_NO_KEY
}

fn decode_key_event(event: &InputEvent) -> Option<(c_int, u8)> {
    if event.event_type != EventType::KeyStateEvent {
        return None;
    }

    let doom_key = doom_key_for_keycodeval(event.value);
    if doom_key == DOOM_KEY_UNMAPPED {
        return None;
    }

    let is_pressed = if event.extra == KeyState::Pressed as u32 {
        KEY_EVENT_PRESSED
    } else {
        KEY_EVENT_RELEASED
    };

    Some((is_pressed, doom_key))
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_SetWindowTitle(_title: *const c_char) {}
