use crate::events::event_buffer::{
    EventBuffer, EventType, InputEvent, KeyModifiers, KeyState, MouseButtons,
};
use crate::events::keyboard_state::{KEY_COUNT, KeyBitset, KeyboardState};
use crate::graphics::FRAMEBUFFER_BYTES_PER_PIXEL;
use crate::graphics::framebuffer::FrameBufferTarget;
use crate::graphics::visible_spans::{
    ComposeLayer, LayerSource, PixelSpanScratch, for_each_visible_span,
};
use crate::graphics::window::{
    INVALID_WINDOW_ID, Window, WindowBounds, WindowBuffer, WindowID, event_buffer_user_vaddr,
};
use crate::memory::paging::{PAGE_SIZE, align_up};
use crate::memory::{get_frame_allocator, usermem};
use crate::process::PID;
use crate::process::process_mem::FrameOwnership;
use crate::process::shared_state::get_shared_program_data_buffer_mut;
use crate::process::{PROCESS_WINDOW_BUFFER_BASE, PROCESS_WINDOW_BUFFER_HARD_MAX_SIZE};
use crate::{serial_println, serial_println_core};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use pc_keyboard::KeyCode;
use spin::{Mutex, MutexGuard, Once, RwLock};
use x86_64::structures::paging::{PageTableFlags, PhysFrame};
use x86_64::{PhysAddr, VirtAddr};

static COMPOSITOR: Once<Mutex<Compositor>> = Once::new();

pub static SHELL_RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn init_compositor(width: u32, height: u32) {
    COMPOSITOR.call_once(|| Mutex::new(Compositor::new(width, height)));
}

pub fn get_compositor() -> MutexGuard<'static, Compositor> {
    COMPOSITOR.get().expect("compositor not initialized").lock()
}

pub fn destroy_windows_of_process(pid: PID) {
    if let Some(compositor) = COMPOSITOR.get() {
        compositor.lock().destroy_windows_of_process(pid);
    }
}

static INPUT_EVENT_BUFFER: Once<EventBuffer> = Once::new();

pub fn init_input_event_buffer() {
    INPUT_EVENT_BUFFER.call_once(EventBuffer::new);
}

pub fn get_input_event_buffer() -> &'static EventBuffer {
    INPUT_EVENT_BUFFER
        .get()
        .expect("input event buffer not initialized")
}

const NORMALIZE_Z_INDEX_THRESHOLD: u8 = 250;

const MOUSE_DEBUG_PRINT: bool = false;
const FOCUS_PREVIOUS_WINDOW_KEY: KeyCode = KeyCode::A;
const FOCUS_NEXT_WINDOW_KEY: KeyCode = KeyCode::D;
const RESTART_SHELL_KEY: KeyCode = KeyCode::T;
pub const CURSOR_COLOR_XRGB: u32 = 0xFFAAAAFF;
pub const CLEAR_COLOR_XRGB: u32 = 0xFF000000;
pub const CURSOR_SIZE_X: i32 = 3;
pub const CURSOR_SIZE_Y: i32 = 10;

pub const WINDOW_PLACEMENT_GAP_PX: i32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlacementSide {
    Right,
    BottomLeft,
    BottomRight,
}

impl PlacementSide {
    /// Tried in this order next to every anchor
    const ORDER: [PlacementSide; 3] = [Self::Right, Self::BottomLeft, Self::BottomRight];

    const fn place_next_to(self, anchor: &WindowBounds, window: WindowBounds) -> WindowBounds {
        let right_x = anchor.right() + WINDOW_PLACEMENT_GAP_PX;
        let below_y = anchor.bottom() + WINDOW_PLACEMENT_GAP_PX;

        match self {
            Self::Right => window.moved_to(right_x, anchor.y),
            Self::BottomLeft => window.moved_to(anchor.x, below_y),
            Self::BottomRight => window.moved_to(right_x, below_y),
        }
    }
}

fn visible_windows_painters_order(windows: &[Window]) -> Vec<&Window> {
    let mut visible_windows: Vec<&Window> =
        windows.iter().filter(|window| window.is_visible).collect();
    visible_windows.sort_unstable_by_key(|window| (window.z_index, window.id));

    visible_windows
}

pub fn find_window_placement(
    requested: WindowBounds,
    occupied: &[WindowBounds],
    screen: WindowBounds,
) -> WindowBounds {
    let is_free = |candidate: &WindowBounds| {
        occupied
            .iter()
            .all(|occupied_bounds| !occupied_bounds.overlaps_window(candidate))
    };
    if is_free(&requested) {
        return requested;
    }

    let anchors = core::iter::once(&requested).chain(occupied.iter());
    for anchor in anchors {
        for side in PlacementSide::ORDER {
            let candidate = side.place_next_to(anchor, requested);
            if screen.contains_window(&candidate) && is_free(&candidate) {
                return candidate;
            }
        }
    }

    requested
}

pub struct Compositor {
    framebuffer_width: u32,
    framebuffer_height: u32,

    next_window_id: AtomicU32,
    currently_focused_window: AtomicU32,
    free_window_ids: Mutex<Vec<WindowID>>,
    // NOTE: cant sort directly by z_index cause the windows are keyed by their id == index
    pub windows: RwLock<Vec<Window>>,

    mouse_x: AtomicI32,
    mouse_y: AtomicI32,
    cursor_visible: AtomicBool,

    /// window ids and screen bounds the last compose drew, in draw order
    drawn_window_layout: Vec<(WindowID, WindowBounds)>,
    drawn_cursor: WindowBounds,
    /// the framebuffer still holds what the bootloader left, the first compose clears it
    is_background_stale: bool,
    compose_layers: Vec<ComposeLayer>,
    span_scratch: PixelSpanScratch,

    keyboard_state: KeyboardState,
}

impl Compositor {
    pub fn new(framebuffer_width: u32, framebuffer_height: u32) -> Self {
        Self {
            framebuffer_width,
            framebuffer_height,
            next_window_id: AtomicU32::new(0),
            currently_focused_window: AtomicU32::new(INVALID_WINDOW_ID),
            windows: RwLock::new(Vec::new()),
            free_window_ids: Mutex::new(Vec::new()),
            mouse_x: AtomicI32::new(0),
            mouse_y: AtomicI32::new(0),
            cursor_visible: AtomicBool::new(true),
            drawn_window_layout: Vec::new(),
            drawn_cursor: WindowBounds::EMPTY,
            is_background_stale: true,
            compose_layers: Vec::new(),
            span_scratch: PixelSpanScratch::new(),
            keyboard_state: KeyboardState::new(),
        }
    }

    pub fn keyboard_state(&self) -> &KeyboardState {
        &self.keyboard_state
    }

    pub fn framebuffer_width(&self) -> u32 {
        self.framebuffer_width
    }

    pub fn framebuffer_height(&self) -> u32 {
        self.framebuffer_height
    }

    pub fn max_window_buffer_bytes(&self) -> u64 {
        let framebuffer_bytes = self.framebuffer_width as u64
            * self.framebuffer_height as u64
            * FRAMEBUFFER_BYTES_PER_PIXEL as u64;

        align_up(framebuffer_bytes, PAGE_SIZE as u64).min(PROCESS_WINDOW_BUFFER_HARD_MAX_SIZE)
    }

    pub fn window_buffer_user_base(&self, window_id: WindowID) -> u64 {
        let slot_size = 2 * self.max_window_buffer_bytes();
        PROCESS_WINDOW_BUFFER_BASE + window_id as u64 * slot_size
    }

    pub fn screen_bounds(&self) -> WindowBounds {
        WindowBounds::new(0, 0, self.framebuffer_width, self.framebuffer_height)
    }

    pub fn visible_window_bounds(&self) -> Vec<WindowBounds> {
        self.windows
            .read()
            .iter()
            .filter(|window| window.is_visible)
            .map(Window::bounds)
            .collect()
    }

    pub fn create_window(
        &self,
        width: u32,
        height: u32,
        x: i32,
        y: i32,
        pid: PID,
    ) -> (WindowID, Arc<WindowBuffer>, Option<&'static EventBuffer>) {
        let placed = find_window_placement(
            WindowBounds::new(x, y, width, height),
            &self.visible_window_bounds(),
            self.screen_bounds(),
        );
        let (x, y) = (placed.x, placed.y);

        let buffer = Arc::new(WindowBuffer::new(width, height, x, y));
        let id = if let Some(free_id) = self.free_window_ids.lock().pop() {
            free_id
        } else {
            self.next_window_id.fetch_add(1, Ordering::Relaxed)
        };

        let (event_buffer, event_buffer_phys) = match self.allocate_event_buffer_for_window(id, pid)
        {
            Some((eb, eb_phys)) => (Some(eb), eb_phys),
            None => (None, PhysAddr::zero()),
        };
        let window = Window {
            id,
            owner_pid: pid,
            x,
            y,
            z_index: 0,
            is_visible: true,
            buffer: buffer.clone(),
            event_buffer,
            event_buffer_phys,
        };

        {
            let mut windows = self.windows.write();
            if id as usize >= windows.len() {
                windows.push(window);
            } else {
                windows[id as usize] = window;
            }
        }

        self.focus_window(id);

        (id, buffer, event_buffer)
    }

    fn allocate_event_buffer_for_window(
        &self,
        window_id: WindowID,
        pid: PID,
    ) -> Option<(&'static EventBuffer, PhysAddr)> {
        serial_println_core!("allocate_event_buffer_for_window: window_id={}", window_id);

        let user_memory_manager = crate::memory::get_user_mem_mgr();
        let mut pm = crate::process::process_manager::PROCESS_MANAGER.lock();
        let mut frame_allocator = get_frame_allocator();
        let (phys, kernel_vaddr) = match usermem::allocate_zeroed_page(&mut frame_allocator) {
            Some((phys, virt)) => (phys, virt),
            None => {
                serial_println_core!(
                    "allocate_event_buffer_for_window: Failed to allocate page for EventBuffer"
                );
                return None;
            }
        };

        unsafe {
            let buffer = &*(kernel_vaddr.as_u64() as *const EventBuffer);
            debug_assert_eq!(buffer.write_idx.load(Ordering::Relaxed), 0);
            debug_assert_eq!(buffer.read_idx.load(Ordering::Relaxed), 0);
            debug_assert_eq!(buffer.event_count.load(Ordering::Relaxed), 0);
        }

        serial_println_core!(
            "allocate_event_buffer_for_window: Allocated EventBuffer for window {}: phys={:?}, kernel_vaddr={:?}",
            window_id,
            phys,
            kernel_vaddr
        );

        serial_println_core!(
            "allocate_event_buffer_for_window: mapping EventBuffer for pid: {}",
            pid
        );
        let Ok(proc) = pm.get_process_mut(pid) else {
            serial_println_core!(
                "allocate_event_buffer_for_window: no process for pid: {}",
                pid
            );
            frame_allocator.deallocate_frame(PhysFrame::containing_address(phys));
            return None;
        };

        if let Err(e) = proc.memory_layout.map_frames(
            VirtAddr::new(event_buffer_user_vaddr(window_id)),
            &[phys],
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
            FrameOwnership::Borrowed,
            user_memory_manager,
            &mut frame_allocator,
        ) {
            serial_println_core!(
                "allocate_event_buffer_for_window: EventBuffer mapping error: {:?}",
                e
            );
            frame_allocator.deallocate_frame(PhysFrame::containing_address(phys));
            return None;
        }
        serial_println_core!("allocate_event_buffer_for_window: end");

        Some((
            unsafe { &*(kernel_vaddr.as_u64() as *const EventBuffer) },
            phys,
        ))
    }

    pub fn set_z_index(&self, window_id: WindowID, z_index: u8) {
        if window_id != INVALID_WINDOW_ID {
            let mut windows = self.windows.write();
            let window = windows.get_mut(window_id as usize).unwrap();
            if window.z_index != z_index {
                window.z_index = z_index;
            }
        }
    }

    pub fn focus_window(&self, window_id: WindowID) {
        if window_id != INVALID_WINDOW_ID {
            let focused_window = self.currently_focused_window.load(Ordering::Acquire);
            if window_id != focused_window {
                let mut windows = self.windows.write();
                let max_z_index = windows.iter().max_by_key(|w| w.z_index).unwrap().z_index;

                match windows.get_mut(window_id as usize) {
                    Some(window) => {
                        window.z_index = max_z_index + 1;
                        let new_event_buffer = window.event_buffer;
                        self.currently_focused_window
                            .store(window_id, Ordering::Release);

                        if max_z_index > NORMALIZE_Z_INDEX_THRESHOLD {
                            let mut visible: Vec<&mut Window> =
                                windows.iter_mut().filter(|w| w.is_visible).collect();
                            visible.sort_by_key(|w| w.z_index);

                            for (i, window) in visible.iter_mut().enumerate() {
                                window.z_index = i as u8;
                            }
                        }

                        let old_window = windows.get(focused_window as usize);
                        if let Some(event_buffer) = old_window.and_then(|w| w.event_buffer) {
                            self.release_held_keys(event_buffer);
                        }
                        if let Some(event_buffer) = new_event_buffer {
                            event_buffer.publish_keyboard_state(
                                &self.keyboard_state.key_down,
                                self.keyboard_state.modifiers,
                            );
                        }

                        unsafe {
                            serial_println_core!(
                                "Compositor: setting focused_window_id to {}",
                                window_id
                            );
                            let shared_program_data = get_shared_program_data_buffer_mut();
                            shared_program_data
                                .focused_window_id
                                .store(window_id, Ordering::Release);
                        }
                    }
                    None => {
                        serial_println_core!(
                            "Compositor: focus_window - window does not exist: {}",
                            window_id
                        );
                    }
                }
            }
        }
    }

    /// The real releases go to the newly focused window, we need to clear the old focued window state
    /// so it doesnt see the held keys as stuck
    fn release_held_keys(&self, event_buffer: &EventBuffer) {
        for key in 0..KEY_COUNT {
            if self.keyboard_state.key_down.test(key as u8) {
                let release_event = InputEvent::new_key_state(
                    key as u32,
                    KeyState::Released,
                    KeyModifiers::empty(),
                );
                let _ = event_buffer.push(release_event);
            }
        }

        event_buffer.publish_keyboard_state(&KeyBitset::new(), KeyModifiers::empty());
    }

    //TODO: actually do something with the invalid window id markings
    /// Hides the window, unmaps its buffers and event page from the owning process and frees
    /// the event page. The window slot is reused by a later create_window
    pub fn destroy_window(&self, window_id: WindowID) {
        if window_id == INVALID_WINDOW_ID {
            serial_println!("Attempted to destroy an invalid window ID: {}", window_id);
            return;
        }

        let was_focused = self.currently_focused_window.load(Ordering::Acquire) == window_id;
        let (owner_pid, event_buffer_phys, buffer_page_count) = {
            let mut windows = self.windows.write();
            let Some(window) = windows.get_mut(window_id as usize) else {
                serial_println!("Attempted to destroy a nonexistent window: {}", window_id);
                return;
            };
            if !window.is_visible {
                return;
            }

            window.is_visible = false;
            let event_buffer_phys = window.event_buffer_phys;
            window.event_buffer = None;
            window.event_buffer_phys = PhysAddr::zero();

            (
                window.owner_pid,
                event_buffer_phys,
                window.buffer.page_count(),
            )
        };
        self.free_window_ids.lock().push(window_id);

        // the dead window kept the focus, without this no window would get keys again
        if was_focused {
            self.focus_topmost_visible_window();
        }

        let user_memory_manager = crate::memory::get_user_mem_mgr();
        let mut pm = crate::process::process_manager::PROCESS_MANAGER.lock();
        let mut frame_allocator = get_frame_allocator();

        if let Ok(proc) = pm.get_process_mut(owner_pid) {
            let layout = &mut proc.memory_layout;
            if layout.top_page_table_phys.as_u64() != 0 {
                let buffer_bytes = (buffer_page_count * PAGE_SIZE) as u64;
                let back_buffer_base = self.window_buffer_user_base(window_id);
                let front_buffer_base = back_buffer_base + self.max_window_buffer_bytes();
                let mut unmap_range = |range_start: u64, range_bytes: u64| {
                    layout.unmap_region(
                        VirtAddr::new(range_start),
                        range_bytes,
                        user_memory_manager,
                        &mut frame_allocator,
                    );
                };

                // buffers that were never mapped by the process are simply not found
                unmap_range(back_buffer_base, buffer_bytes);
                unmap_range(front_buffer_base, buffer_bytes);
                if !event_buffer_phys.is_null() {
                    unmap_range(event_buffer_user_vaddr(window_id), PAGE_SIZE as u64);
                }
            }
        }

        if !event_buffer_phys.is_null() {
            frame_allocator.deallocate_frame(PhysFrame::containing_address(event_buffer_phys));
        }
    }

    fn focus_topmost_visible_window(&self) {
        let topmost_window_id = self
            .windows
            .read()
            .iter()
            .filter(|window| window.is_visible)
            .max_by_key(|window| window.z_index)
            .map_or(INVALID_WINDOW_ID, |window| window.id);

        if topmost_window_id != INVALID_WINDOW_ID {
            self.focus_window(topmost_window_id);
            return;
        }

        self.currently_focused_window
            .store(INVALID_WINDOW_ID, Ordering::Release);
        // SAFETY: a window existed, so userspace and the shared state are initialized, and the
        // reference ends with this function like the one in focus_window
        let shared_program_data = unsafe { get_shared_program_data_buffer_mut() };
        shared_program_data
            .focused_window_id
            .store(INVALID_WINDOW_ID, Ordering::Release);
    }

    /// Run before the process manager lock is taken, window cleanup locks it itself
    pub fn destroy_windows_of_process(&self, pid: PID) {
        let owned_window_ids: Vec<WindowID> = self
            .windows
            .read()
            .iter()
            .filter(|window| window.is_visible && window.owner_pid == pid)
            .map(|window| window.id)
            .collect();

        for window_id in owned_window_ids {
            self.destroy_window(window_id);
        }
    }

    /// True when a window of the process has input waiting in its event buffer
    pub fn is_present_pending(&self, window_id: u32) -> bool {
        self.windows
            .read()
            .get(window_id as usize)
            .is_some_and(|window| {
                window.is_visible && window.buffer.needs_swap.load(Ordering::Acquire)
            })
    }

    pub fn process_has_pending_input(&self, pid: PID) -> bool {
        self.windows.read().iter().any(|window| {
            window.owner_pid == pid
                && window.is_visible
                && window
                    .event_buffer
                    .is_some_and(|event_buffer| !event_buffer.is_empty())
        })
    }

    pub fn process_input_events(&mut self) {
        let buffer = get_input_event_buffer();

        self.keyboard_state.clear_transient();

        let mut forwarded_any_event = false;
        while let Some(event) = buffer.read() {
            forwarded_any_event = true;

            match event.event_type {
                EventType::MouseEvent => {
                    self.handle_mouse_event(event);
                }
                EventType::KeyEvent => {
                    self.handle_keyboard_event(event);
                }
                EventType::KeyStateEvent => {
                    self.handle_key_state_event(event);
                }
                _ => {
                    if let Some(event_buffer) = self.get_focused_window_event_buffer() {
                        let _ = event_buffer.push(event);
                    }
                }
            }
        }

        if forwarded_any_event {
            crate::process::wait::INPUT_WAIT_QUEUE.wake_all();
        }
    }

    fn get_focused_window_event_buffer(&self) -> Option<&'static EventBuffer> {
        let focused_window_id = self.currently_focused_window.load(Ordering::Acquire);
        if focused_window_id == INVALID_WINDOW_ID {
            return None;
        }

        let windows = self.windows.read();
        windows
            .get(focused_window_id as usize)
            .and_then(|window| window.event_buffer)
    }

    fn handle_key_state_event(&mut self, event: InputEvent) {
        debug_assert!((event.value as usize) < KEY_COUNT);
        let key = event.value as u8;
        let state = KeyState::from_raw(event.extra);
        self.keyboard_state
            .update(key, state, event.key_modifiers());

        // letters reach the compositor only as key states and chars, so focus hotkeys are handled here
        if self.keyboard_state.is_down(KeyCode::LAlt) && state == KeyState::Pressed {
            self.handle_alt_hotkey(key);
        }

        if let Some(event_buffer) = self.get_focused_window_event_buffer() {
            event_buffer.publish_keyboard_state(
                &self.keyboard_state.key_down,
                self.keyboard_state.modifiers,
            );
            let _ = event_buffer.push(event);
        }
    }

    fn handle_alt_hotkey(&mut self, key: u8) {
        if key == RESTART_SHELL_KEY as u8 {
            SHELL_RESTART_REQUESTED.store(true, Ordering::Release);
        } else if key == FOCUS_PREVIOUS_WINDOW_KEY as u8 {
            let focused_window_id = self.currently_focused_window.load(Ordering::Acquire);
            let new_focused_window_id = focused_window_id.saturating_sub(1);
            self.focus_window(new_focused_window_id);
        } else if key == FOCUS_NEXT_WINDOW_KEY as u8 {
            let focused_window_id = self.currently_focused_window.load(Ordering::Acquire);
            let (new_focused_window_id, _) = focused_window_id.overflowing_add(1);
            self.focus_window(new_focused_window_id);
        }
    }

    pub fn handle_keyboard_event(&mut self, event: InputEvent) {
        debug_assert!((event.value as usize) < KEY_COUNT);

        if let Some(event_buffer) = self.get_focused_window_event_buffer() {
            let _ = event_buffer.push(event);
        } else {
            serial_println_core!("No focused window event buffer to forward keyboard event");
        }
    }

    fn handle_mouse_event(&mut self, event: InputEvent) {
        let mut mouse_x = self.mouse_x.load(Ordering::Relaxed);
        let mut mouse_y = self.mouse_y.load(Ordering::Relaxed);
        let mouse_event = event.decode_mouse();

        mouse_x += mouse_event.x_delta as i32;
        mouse_y += mouse_event.y_delta as i32;
        mouse_x = mouse_x.clamp(0, self.framebuffer_width as i32 - 1);
        mouse_y = mouse_y.clamp(0, self.framebuffer_height as i32 - 1);

        if MOUSE_DEBUG_PRINT {
            serial_println!(
                "Mouse moved to ({}, {}) with buttons: {:?}",
                mouse_x,
                mouse_y,
                mouse_event.buttons
            );
        }

        self.mouse_x.store(mouse_x, Ordering::Relaxed);
        self.mouse_y.store(mouse_y, Ordering::Relaxed);

        if !mouse_event.buttons.is_empty() {
            self.handle_mouse_click(mouse_x, mouse_y, mouse_event.buttons);
        }

        if let Some(event_buffer) = self.get_focused_window_event_buffer() {
            let _ = event_buffer.push(event);
        } else {
            serial_println_core!("No focused window event buffer to forward mouse event");
        }
    }

    fn handle_mouse_click(&mut self, mouse_x: i32, mouse_y: i32, _buttons: MouseButtons) {
        let focused_window_id = self.currently_focused_window.load(Ordering::Acquire);

        if focused_window_id != INVALID_WINDOW_ID {
            let windows = self.windows.read();
            if let Some(focused_window) = windows.get(focused_window_id as usize)
                && focused_window.is_visible
            {
                let window_x = focused_window.x;
                let window_y = focused_window.y;
                let window_width = focused_window.buffer.width as i32;
                let window_height = focused_window.buffer.height as i32;

                if mouse_x >= window_x
                    && mouse_x < window_x + window_width
                    && mouse_y >= window_y
                    && mouse_y < window_y + window_height
                {
                    serial_println!(
                        "Mouse click within focused window {} at ({}, {})",
                        focused_window_id,
                        mouse_x,
                        mouse_y
                    );
                    return;
                }
            }
        }

        self.find_and_focus_window_at(mouse_x, mouse_y);
    }

    fn find_and_focus_window_at(&self, mouse_x: i32, mouse_y: i32) {
        let target_window_id = {
            let windows = self.windows.read();

            windows
                .iter()
                .filter(|w| w.is_visible)
                .filter(|w| {
                    let x = w.x;
                    let y = w.y;
                    let width = w.buffer.width as i32;
                    let height = w.buffer.height as i32;

                    mouse_x >= x && mouse_x < x + width && mouse_y >= y && mouse_y < y + height
                })
                .max_by_key(|w| w.z_index)
                .map(|w| w.id)
        };

        if let Some(window_id) = target_window_id
            && window_id != self.currently_focused_window.load(Ordering::Acquire)
        {
            self.focus_window(window_id);

            serial_println!("Focused window {} at ({}, {})", window_id, mouse_x, mouse_y);
        }
    }

    //TODO:compositor should own the framebuffer
    //TODO: alpha blending
    pub fn compose(&mut self, framebuffer: &mut FrameBufferTarget) {
        let screen = WindowBounds::new(0, 0, framebuffer.width as u32, framebuffer.height as u32);
        debug_assert!(
            screen.width == self.framebuffer_width && screen.height == self.framebuffer_height
        );

        let windows = self.windows.read();
        let visible_windows = visible_windows_painters_order(&windows);

        let mut swapped_any = false;
        for window in &visible_windows {
            swapped_any |= window.buffer.try_swap();
        }

        let cursor = if self.cursor_visible.load(Ordering::Relaxed) {
            let cursor_x = self.mouse_x.load(Ordering::Relaxed);
            let cursor_y = self.mouse_y.load(Ordering::Relaxed);
            WindowBounds::new(
                cursor_x,
                cursor_y,
                CURSOR_SIZE_X as u32,
                CURSOR_SIZE_Y as u32,
            )
            .clipped_to_area(&screen)
        } else {
            WindowBounds::EMPTY
        };

        self.compose_layers.clear();
        self.compose_layers.push(ComposeLayer::new(
            WindowBounds::EMPTY,
            LayerSource::Background,
        ));

        let mut is_window_layout_same = self.drawn_window_layout.len() == visible_windows.len();
        for (index, window) in visible_windows.iter().enumerate() {
            let bounds = window.bounds().clipped_to_area(&screen);
            self.compose_layers
                .push(ComposeLayer::new(bounds, LayerSource::Window(index)));

            is_window_layout_same &= self
                .drawn_window_layout
                .get(index)
                .is_some_and(|&drawn| drawn == (window.id, bounds));
        }
        self.compose_layers
            .push(ComposeLayer::new(cursor, LayerSource::Cursor));

        // what is not under a window keeps its pixels from the last frame, so the background is
        // only drawn where a window left
        self.compose_layers[0].bounds = if self.is_background_stale || !is_window_layout_same {
            screen
        } else if self.drawn_cursor != cursor {
            self.drawn_cursor
        } else {
            WindowBounds::EMPTY
        };

        if !is_window_layout_same {
            self.drawn_window_layout.clear();
            for (index, window) in visible_windows.iter().enumerate() {
                let bounds = self.compose_layers[index + 1].bounds;
                self.drawn_window_layout.push((window.id, bounds));
            }
        }
        self.drawn_cursor = cursor;
        self.is_background_stale = false;

        let framebuffer_ptr = framebuffer.address();
        let framebuffer_pitch = framebuffer.pitch as usize;
        for_each_visible_span(
            &self.compose_layers,
            &mut self.span_scratch,
            |layer, band_top, band_bottom, span| {
                let rows = band_top as usize..band_bottom as usize;
                let first_column = span.start as usize;
                let row_pixel_count = span.len();

                match layer.source {
                    LayerSource::Window(index) => {
                        let window = visible_windows[index];
                        let source_width = window.buffer.width as usize;
                        let source_column = (span.start - window.x) as usize;
                        let source_first_row = (band_top - window.y) as usize;
                        let source = window.buffer.front_buffer_ptr();

                        for (source_row, row) in (source_first_row..).zip(rows) {
                            // SAFETY: the span lies inside the window and inside the screen
                            unsafe {
                                core::ptr::copy_nonoverlapping(
                                    source.add(source_row * source_width + source_column),
                                    framebuffer_row(framebuffer_ptr, framebuffer_pitch, row)
                                        .add(first_column),
                                    row_pixel_count,
                                );
                            }
                        }
                    }
                    LayerSource::Background => fill_rows(
                        framebuffer_ptr,
                        framebuffer_pitch,
                        rows,
                        first_column,
                        row_pixel_count,
                        CLEAR_COLOR_XRGB,
                    ),
                    LayerSource::Cursor => fill_rows(
                        framebuffer_ptr,
                        framebuffer_pitch,
                        rows,
                        first_column,
                        row_pixel_count,
                        CURSOR_COLOR_XRGB,
                    ),
                }
            },
        );

        if swapped_any {
            crate::process::wait::WINDOW_SWAP_WAIT_QUEUE.wake_all();
        }
    }
}

/// First pixel of a framebuffer row
///
/// # Safety
/// `row` must be below the framebuffer height
unsafe fn framebuffer_row(framebuffer_ptr: *mut u8, pitch: usize, row: usize) -> *mut u32 {
    unsafe { framebuffer_ptr.add(row * pitch).cast::<u32>() }
}

fn fill_rows(
    framebuffer_ptr: *mut u8,
    pitch: usize,
    rows: core::ops::Range<usize>,
    first_column: usize,
    pixel_count: usize,
    color_xrgb: u32,
) {
    for row in rows {
        // SAFETY: the caller keeps the rows and columns inside the framebuffer
        unsafe {
            let row_start = framebuffer_row(framebuffer_ptr, pitch, row).add(first_column);
            core::slice::from_raw_parts_mut(row_start, pixel_count).fill(color_xrgb);
        }
    }
}
