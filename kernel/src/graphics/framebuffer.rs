use core::ptr::NonNull;

use crate::graphics::FRAMEBUFFER_BYTES_PER_PIXEL;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::Size;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use spin::{Mutex, MutexGuard};

pub struct FrameBufferTarget {
    address: NonNull<()>,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bpp: u16,
    pub memory_model: u8,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
}

unsafe impl Send for FrameBufferTarget {}
unsafe impl Sync for FrameBufferTarget {}

impl FrameBufferTarget {
    pub fn from_limine_framebuffer(fb: &limine::framebuffer::Framebuffer) -> Self {
        Self {
            address: NonNull::new(fb.address()).expect("Framebuffer address cannot be null"),
            width: fb.width,
            height: fb.height,
            pitch: fb.pitch,
            bpp: fb.bpp,
            memory_model: fb.memory_model,
            red_mask_size: fb.red_mask_size,
            red_mask_shift: fb.red_mask_shift,
            green_mask_size: fb.green_mask_size,
            green_mask_shift: fb.green_mask_shift,
            blue_mask_size: fb.blue_mask_size,
            blue_mask_shift: fb.blue_mask_shift,
        }
    }

    pub fn address(&self) -> *mut u8 {
        self.address.as_ptr().cast::<u8>()
    }

    pub fn size(&self) -> usize {
        (self.height * self.pitch) as usize
    }

    pub fn write_pixel(&mut self, x: u64, y: u64, color: Rgb888) {
        if x >= self.width || y >= self.height {
            return;
        }

        let px_offset = y * self.pitch + x * FRAMEBUFFER_BYTES_PER_PIXEL as u64;
        let px_ptr = unsafe { self.address().add(px_offset as usize).cast::<u32>() };

        let color: u32 =
            ((color.r() as u32) << 16) | ((color.g() as u32) << 8) | (color.b() as u32);

        unsafe { px_ptr.write(color) };
    }

    /// # Safety
    ///
    /// `src` must be a valid, non-null pointer to at least `width` consecutive `u32` values and
    /// must not alias the destination region in the framebuffer.
    pub unsafe fn copy_row(&mut self, src: *const u32, dst_y: u64, dst_x: u64, width: u64) {
        if dst_y >= self.height || dst_x >= self.width {
            return;
        }

        let copy_width = width.min(self.width - dst_x);
        let dst_offset = dst_y * self.pitch + dst_x * FRAMEBUFFER_BYTES_PER_PIXEL as u64;
        let dst_ptr = unsafe { self.address().add(dst_offset as usize).cast::<u32>() };

        unsafe { core::ptr::copy_nonoverlapping(src, dst_ptr, copy_width as usize) };
    }

    pub fn fill_rect(&mut self, x: u64, y: u64, width: u64, height: u64, color: Rgb888) {
        let x = x.min(self.width);
        let y = y.min(self.height);
        let width = width.min(self.width - x);
        let height = height.min(self.height - y);

        let color_xrgb =
            ((color.r() as u32) << 16) | ((color.g() as u32) << 8) | (color.b() as u32);

        unsafe {
            let fb_addr = self.address();

            for row in 0..height {
                let row_offset = (y + row) * self.pitch + x * FRAMEBUFFER_BYTES_PER_PIXEL as u64;
                let row_ptr = fb_addr.add(row_offset as usize).cast::<u32>();

                for col in 0..width {
                    row_ptr.add(col as usize).write(color_xrgb);
                }
            }
        }
    }
}

impl DrawTarget for FrameBufferTarget {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels {
            self.write_pixel(coord.x as u64, coord.y as u64, color);
        }
        Ok(())
    }
}

impl OriginDimensions for FrameBufferTarget {
    fn size(&self) -> Size {
        Size::new(self.width as u32, self.height as u32)
    }
}

static FRAMEBUFFER_TARGET: spin::Once<Mutex<FrameBufferTarget>> = spin::Once::new();

pub fn init_framebuffer(fb: &limine::framebuffer::Framebuffer) {
    let target = FrameBufferTarget::from_limine_framebuffer(fb);
    FRAMEBUFFER_TARGET.call_once(|| Mutex::new(target));
}

pub fn get_framebuffer() -> MutexGuard<'static, FrameBufferTarget> {
    FRAMEBUFFER_TARGET
        .get()
        .expect("Framebuffer not initialized")
        .lock()
}
