use crate::graphics::FRAMEBUFFER_BYTES_PER_PIXEL;
use crate::graphics::framebuffer::FrameBufferTarget;
use crate::graphics::window::{INVALID_WINDOW_ID, Window, WindowBuffer, WindowID};
use crate::serial_println;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::{Mutex, RwLock};

const NORMALIZE_Z_INDEX_THRESHOLD: u8 = 250;
const DEBUG_INFO: bool = false;

pub struct Compositor {
    next_window_id: AtomicU32,
    currently_focused_window: Mutex<WindowID>,
    free_window_ids: Mutex<Vec<WindowID>>,
    windows: RwLock<Vec<Window>>,
}

impl Default for Compositor {
    fn default() -> Self {
        Self::new()
    }
}

impl Compositor {
    pub fn new() -> Self {
        Self {
            next_window_id: AtomicU32::new(0),
            currently_focused_window: Mutex::new(0),
            windows: RwLock::new(Vec::new()),
            free_window_ids: Mutex::new(Vec::new()),
        }
    }

    pub fn create_window(
        &self,
        width: u32,
        height: u32,
        x: i32,
        y: i32,
    ) -> (WindowID, Arc<WindowBuffer>) {
        let buffer = Arc::new(WindowBuffer::new(width, height, x, y));
        let id = if let Some(free_id) = self.free_window_ids.lock().pop() {
            free_id
        } else {
            self.next_window_id.fetch_add(1, Ordering::Relaxed)
        };

        let window = Window {
            id,
            x,
            y,
            z_index: 0,
            is_visible: true,
            buffer: buffer.clone(),
        };

        let mut windows = self.windows.write();
        if id as usize >= windows.len() {
            windows.push(window);
        } else {
            windows[id as usize] = window;
        }

        *self.currently_focused_window.lock() = id;

        (id, buffer)
    }

    pub fn set_z_index(&self, window_id: WindowID, z_index: u8) {
        if window_id != INVALID_WINDOW_ID {
            let mut windows = self.windows.write();
            let window = windows.get_mut(window_id as usize).unwrap();
            window.z_index = z_index;
        }
    }

    pub fn focus_window(&self, window_id: WindowID) {
        if window_id != INVALID_WINDOW_ID {
            let mut focused_window = self.currently_focused_window.lock();
            if window_id != *focused_window {
                let mut windows = self.windows.write();
                let max_z_index = windows.iter().map(|w| w.z_index).max().unwrap_or(0); //TODO: maybe cache this

                let window = windows.get_mut(window_id as usize).unwrap();
                window.z_index = max_z_index + 1;
                *focused_window = window_id;

                if max_z_index > NORMALIZE_Z_INDEX_THRESHOLD {
                    let mut visible: Vec<&mut Window> =
                        windows.iter_mut().filter(|w| w.is_visible).collect();
                    visible.sort_by_key(|w| w.z_index);

                    for (i, window) in visible.iter_mut().enumerate() {
                        window.z_index = i as u8;
                    }
                }
            }
        }
    }

    //TODO: actually do something with the invalid window id markings
    pub fn destroy_window(&self, window_id: WindowID) {
        if window_id != INVALID_WINDOW_ID {
            let mut windows = self.windows.write();
            let window = windows.get_mut(window_id as usize).unwrap();
            window.is_visible = false;
            self.free_window_ids.lock().push(window_id);
        } else {
            serial_println!("Attempted to destroy an invalid window ID: {}", window_id);
        }
    }

    //TODO:compositor should own the framebuffer
    //TODO: alpha blending
    pub fn compose(&self, framebuffer: &mut FrameBufferTarget) {
        let windows = self.windows.read();

        let mut visible_windows: Vec<&Window> = windows.iter().filter(|w| w.is_visible).collect();
        visible_windows.sort_by_key(|w| w.z_index);

        let framebuffer_ptr = framebuffer.address();
        let framebuffer_pitch = framebuffer.pitch;
        let framebuffer_width = framebuffer.width;
        let framebuffer_height = framebuffer.height;

        if DEBUG_INFO {
            serial_println!(
                "Composing frame with {} visible windows",
                visible_windows.len()
            );
        }

        // TODO: clear the framebufer

        for window in &visible_windows {
            if window.is_visible {
                window.buffer.try_swap();
                window.buffer.draw_border();

                if DEBUG_INFO {
                    serial_println!(
                        "Compositing window ID {} at position ({}, {}) with size {}x{}",
                        window.id,
                        window.x,
                        window.y,
                        window.buffer.width,
                        window.buffer.height
                    );
                }

                let start_x = window.x.max(0) as u32;
                let start_y = window.y.max(0) as u32;
                let end_x =
                    (window.x + window.buffer.width as i32).min(framebuffer_width as i32) as u32;
                let end_y =
                    (window.y + window.buffer.height as i32).min(framebuffer_height as i32) as u32;

                if start_x >= end_x || start_y >= end_y {
                    serial_println!("Skipping window ID {} - out of bounds", window.id);
                    continue;
                }

                let src_x = if window.x > 0 { 0 } else { -window.x as u32 };
                let src_y = if window.y > 0 { 0 } else { -window.y as u32 };

                let copy_width = end_x - start_x;
                let copy_height = end_y - start_y;

                let src_ptr = window.buffer.front_buffer_ptr();

                if DEBUG_INFO {
                    serial_println!(
                        "Copying window ID {} to framebuffer region ({}, {}) - ({}, {})",
                        window.id,
                        start_x,
                        start_y,
                        end_x,
                        end_y
                    );
                }

                for y in 0..copy_height {
                    let src_offset = ((src_y + y) * window.buffer.width + src_x) as usize;
                    let dst_offset = ((start_y + y) * framebuffer_pitch as u32
                        + start_x * FRAMEBUFFER_BYTES_PER_PIXEL)
                        as usize;

                    unsafe {
                        let dst_ptr = framebuffer_ptr.add(dst_offset).cast::<u32>();
                        core::ptr::copy_nonoverlapping(
                            src_ptr.add(src_offset),
                            dst_ptr,
                            copy_width as usize,
                        );
                    }
                }
            }
        }
    }
}
