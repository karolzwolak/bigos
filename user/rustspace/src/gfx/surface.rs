use crate::gfx::color::{Rgba8888UNORM, rgba_to_xrgb};
use core::arch::x86_64::__m128i;
use core::arch::x86_64::_mm_set1_epi32;
use core::arch::x86_64::_mm_storeu_si128;
use embedded_graphics::Pixel;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::{Dimensions, DrawTarget, OriginDimensions, Point, Size};
use embedded_graphics::primitives::Rectangle;

/// Userspace pixel surface backed by a kernel-mapped back buffer.
/// `pixels` points to the user virtual address returned by sys_map_window_buffer.
pub struct UserSurface {
    pub pixels: *mut u32,
    pub pixels_second: *mut u32,
    pub width: u32,
    pub height: u32,
}

impl UserSurface {
    /// Construct from the raw pointer and dimensions returned by sys_map_window_buffer
    /// + sys_get_window_size.
    ///
    /// # Safety
    /// `pixels` must be a valid, writable mapping of `width * height` u32 slots.
    pub unsafe fn new(pixels: *mut u32, pixels_second: *mut u32, width: u32, height: u32) -> Self {
        Self {
            pixels,
            pixels_second,
            width,
            height,
        }
    }

    pub unsafe fn swap(&mut self) {
        core::mem::swap(&mut self.pixels, &mut self.pixels_second);
    }

    #[inline(always)]
    pub fn write_pixel(&mut self, x: u32, y: u32, color: Rgba8888UNORM) {
        if x < self.width && y < self.height {
            let offset = (y * self.width + x) as usize;
            unsafe { *self.pixels.add(offset) = rgba_to_xrgb(color) };
        }
    }

    /// Bounds-unchecked pixel write for hot paths. Caller must ensure x < width and y < height.
    #[inline(always)]
    pub unsafe fn write_pixel_unchecked(&mut self, x: u32, y: u32, color: Rgba8888UNORM) {
        let offset = (y * self.width + x) as usize;
        unsafe { *self.pixels.add(offset) = rgba_to_xrgb(color) };
    }

    // pub fn clear(&mut self, color: Rgba8888UNORM) {
    //     let xrgb = rgba_to_xrgb(color);
    //     let count = (self.width * self.height) as usize;
    //     for i in 0..count {
    //         unsafe { *self.pixels.add(i) = xrgb };
    //     }
    // }

    #[inline]
    pub fn clear(&mut self, color: Rgba8888UNORM) {
        unsafe {
            let mut buffer = self.pixels;
            let len = self.width * self.height;
            let color_u32 = color.to_u32_xrgb();

            let color_vec = _mm_set1_epi32(color_u32 as i32);

            let mut i = 0;
            while i + 4 <= len {
                let ptr = buffer.add(i as usize) as *mut __m128i;
                _mm_storeu_si128(ptr, color_vec);
                i += 4;
            }
            for j in i..len {
                *buffer.add(j as usize) = color_u32;
            }
        }
    }

    pub fn as_slice_mut(&mut self) -> &mut [u32] {
        unsafe { core::slice::from_raw_parts_mut(self.pixels, (self.width * self.height) as usize) }
    }
}

// UserSurface wraps a raw pointer — it is only ever used from a single process/thread.
unsafe impl Send for UserSurface {}
unsafe impl Sync for UserSurface {}

impl DrawTarget for UserSurface {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels {
            if coord.x >= 0 && coord.y >= 0 {
                self.write_pixel(
                    coord.x as u32,
                    coord.y as u32,
                    Rgba8888UNORM::from_rgb_emb(color),
                );
            }
        }
        Ok(())
    }
}

impl OriginDimensions for UserSurface {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}
