use embedded_graphics::pixelcolor::{Rgb888, RgbColor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba8888UNORM {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8888UNORM {
    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    pub const BLACK: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    pub const RED: Self = Self {
        r: 255,
        g: 0,
        b: 0,
        a: 255,
    };
    pub const GREEN: Self = Self {
        r: 0,
        g: 255,
        b: 0,
        a: 255,
    };
    pub const BLUE: Self = Self {
        r: 0,
        g: 0,
        b: 255,
        a: 255,
    };
    pub const GRAY: Self = Self {
        r: 64,
        g: 64,
        b: 64,
        a: 255,
    };

    pub fn from_rgb_emb(rgb: Rgb888) -> Self {
        Self {
            r: rgb.r(),
            g: rgb.g(),
            b: rgb.b(),
            a: 255,
        }
    }

    pub fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub fn from_rgbf32(r: f32, g: f32, b: f32) -> Self {
        Self {
            r: (r * 255.0) as u8,
            g: (g * 255.0) as u8,
            b: (b * 255.0) as u8,
            a: 255,
        }
    }

    pub fn from_u32_rgba8(rgba: u32) -> Self {
        Self {
            r: ((rgba >> 24) & 0xFF) as u8,
            g: ((rgba >> 16) & 0xFF) as u8,
            b: ((rgba >> 8) & 0xFF) as u8,
            a: (rgba & 0xFF) as u8,
        }
    }

    pub fn to_u32_rgba(&self) -> u32 {
        ((self.r as u32) << 24) | ((self.g as u32) << 16) | ((self.b as u32) << 8) | (self.a as u32)
    }

    pub fn from_u32_xrgb(xrgb: u32) -> Self {
        Self {
            r: ((xrgb >> 16) & 0xFF) as u8,
            g: ((xrgb >> 8) & 0xFF) as u8,
            b: (xrgb & 0xFF) as u8,
            a: 255,
        }
    }

    pub fn to_u32_xrgb(&self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    pub fn blend_over(&self, dest: Rgb888) -> Rgb888 {
        if self.a == 255 {
            return Rgb888::new(self.r, self.g, self.b);
        } else if self.a == 0 {
            return dest;
        }

        let alpha = self.a as f32 / 255.0;
        let inv_alpha = 1.0 - alpha;
        Rgb888::new(
            (self.r as f32 * alpha + dest.r() as f32 * inv_alpha) as u8,
            (self.g as f32 * alpha + dest.g() as f32 * inv_alpha) as u8,
            (self.b as f32 * alpha + dest.b() as f32 * inv_alpha) as u8,
        )
    }
}

pub const fn rgba_to_xrgb(color: Rgba8888UNORM) -> u32 {
    ((color.r as u32) << 16) | ((color.g as u32) << 8) | (color.b as u32)
}

pub const fn xrgb_to_rgba(xrgb: u32) -> Rgba8888UNORM {
    Rgba8888UNORM {
        r: ((xrgb >> 16) & 0xFF) as u8,
        g: ((xrgb >> 8) & 0xFF) as u8,
        b: (xrgb & 0xFF) as u8,
        a: 255,
    }
}
