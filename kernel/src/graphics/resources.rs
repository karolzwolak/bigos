use crate::graphics::color::Rgba8888UNORM;
use crate::graphics::window::{WindowBackBuffer, WindowBuffer};
use crate::serial_println;
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::x86_64::*;

//NOTE: also consider compression at some point in the far future

#[derive(Clone)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub data: Arc<Vec<u32>>, //TODO assuming RGBA8 for now
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
        let index = (y * self.width) + x;
        let pixel = self.data[index as usize];

        Rgba8888UNORM::from_u32_rgba8(pixel)
    }

    #[inline(always)]
    pub fn sample(&self, x: u32, y: u32) -> Rgba8888UNORM {
        //TODO clamp, repeat sampling modes
        if x < self.width && y < self.height {
            let index = (y * self.width) + x;
            Rgba8888UNORM::from_u32_rgba8(self.data[index as usize])
        } else {
            Rgba8888UNORM::BLACK
        }
    }
}

#[derive(Clone)]
pub struct ConstantBuffer {
    pub data: Arc<Vec<u8>>, //TODO for now we only have raw buffers, would be useful to have formatted buffers
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
            data: Arc::new(data.clone()),
        }
    }
}

pub struct DepthBuffer {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>, //TODO: dont use Vec here and above,
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
    pub fn test(&mut self, x: u32, y: u32, val: f32) -> bool {
        let idx = (y * self.width + x) as usize;
        return val < self.data[idx];
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
        self.data.fill(f32::MAX)
    }
}
