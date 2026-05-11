use crate::graphics::FRAMEBUFFER_BYTES_PER_PIXEL;
use crate::graphics::color::{Rgba8888UNORM, rgba_to_xrgb, xrgb_to_rgba};
use crate::memory::paging::PAGE_SIZE;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use embedded_graphics::Pixel;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::{Dimensions, DrawTarget, Point, Size};
use embedded_graphics::primitives::Rectangle;

pub type WindowID = u32;
pub const INVALID_WINDOW_ID: WindowID = u32::MAX;

pub struct Window {
    pub id: WindowID,
    pub x: i32,
    pub y: i32,
    pub z_index: u8,
    pub is_visible: bool,

    pub buffer: Arc<WindowBuffer>,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(&self, px: u32, py: u32) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }

    pub fn get_intersection_rect(&self, other: &Rect) -> Option<Rect> {
        let self_right = self.x + self.width;
        let self_bottom = self.y + self.height;
        let other_right = other.x + other.width;
        let other_bottom = other.y + other.height;

        if self_right <= other.x
            || self.x >= other_right
            || self_bottom <= other.y
            || self.y >= other_bottom
        {
            return None;
        }

        let x1 = self.x.max(other.x);
        let y1 = self.y.max(other.y);
        let x2 = self_right.min(other_right);
        let y2 = self_bottom.min(other_bottom);

        Some(Rect::new(x1, y1, x2 - x1, y2 - y1))
    }

    pub fn get_union_rect(&self, other: &Rect) -> Rect {
        let self_right = self.x + self.width;
        let self_bottom = self.y + self.height;
        let other_right = other.x + other.width;
        let other_bottom = other.y + other.height;

        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let x2: u32 = self_right.max(other_right);
        let y2 = self_bottom.max(other_bottom);

        Rect {
            x,
            y,
            width: x2 - x,
            height: y2 - y,
        }
    }
}

// SAFETY: WindowBuffer manages two pixel buffers with access split between writer (back) and
// reader (front). Concurrent access is coordinated via `needs_swap` (AtomicBool) and the
// `try_swap` method. Raw pointer access must be externally synchronized by the caller.
unsafe impl Send for WindowBuffer {}
unsafe impl Sync for WindowBuffer {}

pub struct WindowBuffer {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,

    pub back_buffer: UnsafeCell<NonNull<u32>>, // written to
    pub front_buffer: UnsafeCell<NonNull<u32>>, // read by the compositor

    pub needs_swap: AtomicBool, // if the content is not dirty, we dont have to swap //TODO:

    pub swap_count: AtomicU32, // debug
}

impl WindowBuffer {
    pub fn new(width: u32, height: u32, x: i32, y: i32) -> Self {
        let pixel_count = (width * height) as usize;
        let buffer_size = pixel_count * FRAMEBUFFER_BYTES_PER_PIXEL as usize;

        let layout = core::alloc::Layout::from_size_align(buffer_size, PAGE_SIZE).unwrap();
        let back_buffer_ptr = unsafe { alloc::alloc::alloc_zeroed(layout) as *mut u32 };
        let front_buffer_ptr = unsafe { alloc::alloc::alloc_zeroed(layout) as *mut u32 };

        Self {
            width,
            height,
            x,
            y,
            back_buffer: UnsafeCell::new(
                NonNull::new(back_buffer_ptr).expect("Failed to allocate back buffer"),
            ),
            front_buffer: UnsafeCell::new(
                NonNull::new(front_buffer_ptr).expect("Failed to allocate front buffer"),
            ),
            needs_swap: AtomicBool::new(false),
            swap_count: AtomicU32::new(0),
        }
    }

    /// the process should call this to signal that the backbuffer is ready to be presented
    pub fn present(&self) {
        self.needs_swap.store(true, Ordering::Release);
    }

    pub fn back_buffer_mut(&self) -> WindowBackBuffer<'_> {
        WindowBackBuffer { window: self }
    }

    pub fn front_buffer(&self) -> WindowPresentBuffer<'_> {
        WindowPresentBuffer { window: self }
    }

    pub fn try_swap(&self) -> bool {
        if !self.needs_swap.load(Ordering::Acquire) {
            return false;
        }

        unsafe {
            let back = *self.back_buffer.get();
            let front = *self.front_buffer.get();
            *self.back_buffer.get() = front;
            *self.front_buffer.get() = back;
        }

        self.needs_swap.store(false, Ordering::Release);
        self.swap_count.fetch_add(1, Ordering::Relaxed);

        true
    }

    pub fn back_buffer_ptr(&self) -> *mut u32 {
        unsafe { (*self.back_buffer.get()).as_ptr() }
    }

    // Helper to get front buffer pointer
    pub fn front_buffer_ptr(&self) -> *const u32 {
        unsafe { (*self.front_buffer.get()).as_ptr() }
    }

    pub fn get_bounding_rect(&self) -> Rect {
        Rect::new(self.x as u32, self.y as u32, self.width, self.height)
    }
}

pub struct WindowBackBuffer<'a> {
    pub window: &'a WindowBuffer,
}

pub struct WindowPresentBuffer<'a> {
    pub window: &'a WindowBuffer,
}

impl<'a> WindowBackBuffer<'a> {
    pub fn write_pixel(&mut self, x: u32, y: u32, color: Rgba8888UNORM) {
        if x < self.window.width && y < self.window.height {
            let offset = (y * self.window.width + x) as usize;
            let xrgb = rgba_to_xrgb(color);
            unsafe {
                *self.window.back_buffer_ptr().add(offset) = xrgb;
            }
            self.window.needs_swap.store(true, Ordering::Release);
        }
    }

    /// # Safety
    ///
    /// `x` must be less than `self.window.width` and `y` must be less than `self.window.height`.
    /// No bounds checking is performed.
    pub unsafe fn write_pixel_unchecked(&mut self, x: u32, y: u32, color: Rgba8888UNORM) {
        let offset = (y * self.window.width + x) as usize;
        let xrgb = rgba_to_xrgb(color);
        unsafe {
            *self.window.back_buffer_ptr().add(offset) = xrgb;
        }
        self.window.needs_swap.store(true, Ordering::Release);
    }

    pub fn clear(&mut self, color: Rgba8888UNORM) {
        let xrgb = rgba_to_xrgb(color);
        let pixel_count = (self.window.width * self.window.height) as usize;
        for i in 0..pixel_count {
            unsafe {
                *self.window.back_buffer_ptr().add(i) = xrgb;
            }
        }
        self.window.needs_swap.store(true, Ordering::Release);
    }

    pub fn as_slice_mut(&mut self) -> &mut [u32] {
        unsafe {
            core::slice::from_raw_parts_mut(
                self.window.back_buffer_ptr(),
                (self.window.width * self.window.height) as usize,
            )
        }
    }
}

impl<'a> WindowPresentBuffer<'a> {
    pub fn read_pixel(&self, x: u32, y: u32) -> Rgba8888UNORM {
        if x < self.window.width && y < self.window.height {
            let offset = (y * self.window.width + x) as usize;
            let xrgb = unsafe { *self.window.front_buffer_ptr().add(offset) };
            xrgb_to_rgba(xrgb)
        } else {
            Rgba8888UNORM::BLACK
        }
    }

    /// # Safety
    ///
    /// `x` must be less than `self.window.width` and `y` must be less than `self.window.height`.
    /// No bounds checking is performed.
    pub unsafe fn read_pixel_unchecked(&self, x: u32, y: u32) -> Rgba8888UNORM {
        let offset = (y * self.window.width + x) as usize;
        let xrgb = unsafe { *self.window.front_buffer_ptr().add(offset) };
        xrgb_to_rgba(xrgb)
    }

    pub fn as_slice(&self) -> &[u32] {
        unsafe {
            core::slice::from_raw_parts(
                self.window.front_buffer_ptr(),
                (self.window.width * self.window.height) as usize,
            )
        }
    }
}

impl Drop for WindowBuffer {
    fn drop(&mut self) {
        let pixel_count = (self.width * self.height) as usize;
        let buffer_size = pixel_count * FRAMEBUFFER_BYTES_PER_PIXEL as usize;
        let layout = core::alloc::Layout::from_size_align(buffer_size, PAGE_SIZE).unwrap();
        unsafe {
            alloc::alloc::dealloc(self.back_buffer_ptr() as *mut u8, layout);
            alloc::alloc::dealloc(self.front_buffer_ptr() as *mut u8, layout);
        }
    }
}

impl<'a> DrawTarget for WindowBackBuffer<'a> {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels {
            let rgba_color = Rgba8888UNORM::from_rgb_emb(color);
            self.write_pixel(coord.x as u32, coord.y as u32, rgba_color);
        }
        Ok(())
    }

    // fn fill_contiguous<I>(&mut self, area: &embedded_graphics::primitives::Rectangle, colors: I) -> Result<(), Self::Error>
    // where
    //     I: IntoIterator<Item = Self::Color>,
    // {
    //     self.draw_iter(
    //         area.points()
    //             .zip(colors)
    //             .map(|(pos, color)| Pixel(pos, color)),
    //     )
    // }

    // fn fill_solid(&mut self, area: &embedded_graphics::primitives::Rectangle, color: Self::Color) -> Result<(), Self::Error> {
    //     self.fill_contiguous(area, core::iter::repeat(color))
    // }

    // fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
    //     self.fill_solid(&self.bounding_box(), color)
    // }
}

impl<'a> Dimensions for WindowBackBuffer<'a> {
    fn bounding_box(&self) -> Rectangle {
        Rectangle::new(
            Point::new(self.window.x, self.window.y),
            Size::new(self.window.width, self.window.height),
        )
    }
}
