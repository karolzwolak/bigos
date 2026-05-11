use crate::graphics::FRAMEBUFFER_BYTES_PER_PIXEL;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::Size;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use limine::framebuffer::Framebuffer;
use spin::MutexGuard;

pub struct FrameBufferTarget<'a> {
    pub framebuffer: MutexGuard<'a, Framebuffer<'static>>,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
}

impl<'a> FrameBufferTarget<'a> {
    pub fn new(framebuffer: MutexGuard<'a, Framebuffer<'static>>) -> Self {
        let width = framebuffer.width();
        let height = framebuffer.height();
        let pitch = framebuffer.pitch();
        Self {
            framebuffer,
            width,
            height,
            pitch,
        }
    }

    fn write_pixel(&mut self, x: u64, y: u64, color: Rgb888) {
        if x >= self.width || y >= self.height {
            return;
        }

        let px_offset = y * self.pitch + x * 4;
        let px_ptr = unsafe {
            self.framebuffer
                .addr()
                .add(px_offset as usize)
                .cast::<u32>()
        };

        let color: u32 =
            ((color.r() as u32) << 16) | ((color.g() as u32) << 8) | (color.b() as u32);

        unsafe { px_ptr.write(color) };
    }

    pub fn width(&self) -> u64 {
        self.width
    }

    pub fn height(&self) -> u64 {
        self.height
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
        let dst_ptr = unsafe {
            self.framebuffer
                .addr()
                .add(dst_offset as usize)
                .cast::<u32>()
        };

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
            let fb_addr = self.framebuffer.addr();

            for row in 0..height {
                let row_offset = (y + row) * self.pitch + x * 4;
                let row_ptr = fb_addr.add(row_offset as usize).cast::<u32>();

                for col in 0..width {
                    row_ptr.add(col as usize).write(color_xrgb);
                }
            }
        }
    }
}

impl<'a> DrawTarget for FrameBufferTarget<'a> {
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

impl<'a> OriginDimensions for FrameBufferTarget<'a> {
    fn size(&self) -> Size {
        Size::new(self.width as u32, self.height as u32)
    }
}
