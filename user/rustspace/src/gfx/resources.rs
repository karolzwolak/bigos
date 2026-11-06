use crate::gfx::color::Rgba8888UNORM;
use alloc::sync::Arc;
use alloc::vec::Vec;

#[derive(Clone)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub data: Arc<Vec<u32>>, // RGBA8 packed
}

impl Texture {
    pub fn new(width: u32, height: u32) -> Self {
        let size = (width * height) as usize;
        Self {
            width,
            height,
            data: Arc::new(alloc::vec![0u32; size]),
        }
    }

    pub fn from_data(width: u32, height: u32, data: Vec<u32>) -> Self {
        Self {
            data: Arc::new(data),
            width,
            height,
        }
    }

    #[inline(always)]
    pub fn sample_nearest(&self, u: f32, v: f32) -> Rgba8888UNORM {
        let x = (u * self.width as f32) as u32 % self.width;
        let y = (v * self.height as f32) as u32 % self.height;
        Rgba8888UNORM::from_u32_rgba8(self.data[(y * self.width + x) as usize])
    }

    #[inline(always)]
    pub fn sample(&self, x: u32, y: u32) -> Rgba8888UNORM {
        if x < self.width && y < self.height {
            Rgba8888UNORM::from_u32_rgba8(self.data[(y * self.width + x) as usize])
        } else {
            Rgba8888UNORM::BLACK
        }
    }
}

#[derive(Clone)]
pub struct ConstantBuffer {
    pub data: Arc<Vec<u8>>,
}

impl ConstantBuffer {
    pub fn new(size: usize) -> Self {
        Self {
            data: Arc::new(alloc::vec![0u8; size]),
        }
    }

    pub fn from_data(data: Vec<u8>) -> Self {
        Self {
            data: Arc::new(data),
        }
    }
}

#[derive(Clone)]
pub struct RWBuffer {
    pub data: Arc<Vec<u8>>,
}

impl RWBuffer {
    pub fn new(size: usize) -> Self {
        Self {
            data: Arc::new(alloc::vec![0u8; size]),
        }
    }

    pub fn from_data(data: Vec<u8>) -> Self {
        Self {
            data: Arc::new(data),
        }
    }
}

pub struct DepthBuffer {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
}

impl DepthBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: alloc::vec![f32::MAX; (width * height) as usize],
        }
    }

    #[inline(always)]
    pub fn test(&self, x: u32, y: u32, val: f32) -> bool {
        val < self.data[(y * self.width + x) as usize]
    }

    #[inline(always)]
    pub fn test_and_set(&mut self, x: u32, y: u32, val: f32) -> bool {
        let idx = (y * self.width + x) as usize;
        if val < self.data[idx] {
            self.data[idx] = val;
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) {
        self.data.fill(f32::MAX);
    }
}
