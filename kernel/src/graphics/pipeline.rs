use crate::graphics::color::Rgba8888UNORM;
use crate::graphics::resources::{ConstantBuffer, Texture};
use crate::graphics::window::WindowBackBuffer;
use alloc::boxed::Box;
use core::simd::{f32x2, f32x4};

#[derive(Debug, Clone, Copy)]
#[repr(C, align(16))]
pub struct Vertex2D {
    pub xyuv: f32x4,
}

impl Vertex2D {
    pub const fn new(x: f32, y: f32, u: f32, v: f32) -> Self {
        Self {
            xyuv: f32x4::from_array([x, y, u, v]),
        }
    }

    pub fn x(&self) -> f32 {
        self.xyuv[0]
    }
    pub fn y(&self) -> f32 {
        self.xyuv[1]
    }
    pub fn u(&self) -> f32 {
        self.xyuv[2]
    }
    pub fn v(&self) -> f32 {
        self.xyuv[3]
    }

    pub fn as_array(&self) -> [f32; 4] {
        self.xyuv.to_array()
    }

    pub fn as_simd(&self) -> f32x4 {
        self.xyuv
    }

    /// Process 4 vertices in one SIMD operation
    pub fn load_four(vertices: &[Vertex2D; 4]) -> [f32x4; 4] {
        [
            vertices[0].xyuv,
            vertices[1].xyuv,
            vertices[2].xyuv,
            vertices[3].xyuv,
        ]
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C, align(16))]
pub struct Vertex3D {
    pub pos: f32x4,  // [x, y, z, w] - homogeneous position
    pub uv: f32x4,   // [u, v, 0, 0]
    pub norm: f32x4, // [nx, ny, nz, 0] - normal vector
}

impl Vertex3D {
    pub fn new(pos: f32x4, uv: f32x4, norm: f32x4) -> Self {
        Self { pos, uv, norm }
    }

    pub fn position(&self) -> f32x4 {
        self.pos
    }
    pub fn uv(&self) -> f32x4 {
        self.uv
    }
    pub fn normal(&self) -> f32x4 {
        self.norm
    }
}

#[derive(Clone)]
pub struct VertexLayout {
    pub stride: u8,
    pub offset: usize,
}

impl VertexLayout {
    pub fn new_2d() -> Self {
        Self {
            stride: 8,
            offset: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
#[repr(u8)]
pub enum CullMode {
    None,
    Back,
    Front,
}

#[derive(Clone, Debug)]
pub struct RasterizerState {
    pub cull_mode: CullMode,
}

impl Default for RasterizerState {
    fn default() -> Self {
        Self {
            cull_mode: CullMode::None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum BlendFactor {
    One,
    Zero,
    SrcAlpha,
    OneMinusSrcAlpha,
}

#[derive(Clone, Debug)]
pub struct BlendState {
    pub enabled: bool,
    pub src_factor: BlendFactor,
    pub dst_factor: BlendFactor,
}

impl Default for BlendState {
    fn default() -> Self {
        Self {
            enabled: false,
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::Zero,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RenderMode {
    XY,
    XYZ,
}

pub struct PipelineState {
    pub vs: Box<dyn VertexShader>,
    pub ps: Box<dyn PixelShader>,
    pub vertex_layout: VertexLayout,
    pub rasterizer_state: RasterizerState,
    pub blend_state: BlendState,
    pub render_mode: RenderMode,
}

pub struct VSIn<'a> {
    pub vertex_data: &'a [u8],
    pub vertex_id: u32,
    pub instance_id: u32,
}

#[derive(Clone, Debug)]
pub struct VSOut {
    pub position: f32x4,   // [x, y, z, w] - homogeneous position
    pub attributes: f32x4, // up to 4 interpolated attributes (u, v, ...)
    pub extra: f32x4,      // up to 4 interpolated attributes
}

impl VSOut {
    pub fn with_pos_uv(position: f32x4, uv: f32x2) -> Self {
        Self {
            position,
            attributes: f32x4::from_array([uv[0], uv[1], 0.0, 0.0]),
            extra: f32x4::splat(0.0),
        }
    }

    pub fn with_attributes(position: f32x4, attributes: f32x4, extra: f32x4) -> Self {
        Self {
            position,
            attributes,
            extra,
        }
    }

    pub fn uv(&self) -> f32x2 {
        f32x2::from_array([self.attributes[0], self.attributes[1]])
    }

    pub fn from_xyuv(xyuv: &f32x4) -> Self {
        Self {
            position: f32x4::from_array([(*xyuv)[0], (*xyuv)[1], 0.0, 1.0]),
            attributes: f32x4::from_array([(*xyuv)[2], (*xyuv)[3], 0.0, 0.0]),
            extra: f32x4::splat(0.0),
        }
    }

    pub fn x(&self) -> f32 {
        self.position[0]
    }
    pub fn y(&self) -> f32 {
        self.position[1]
    }
    pub fn z(&self) -> f32 {
        self.position[2]
    }
    pub fn w(&self) -> f32 {
        self.position[3]
    }
    pub fn u(&self) -> f32 {
        self.attributes[0]
    }
    pub fn v(&self) -> f32 {
        self.attributes[1]
    }
}

pub trait VertexShader: Send + Sync {
    fn run(&self, input: &VSIn, output: &mut VSOut, constants: &[ConstantBuffer]);
}

pub struct PSIn<'a> {
    pub attributes: f32x4,
    pub screen_x: u16,
    pub screen_y: u16,
    pub render_target: &'a mut [u32], //TODO: multiple render targets
    pub textures: &'a [Texture],
    pub constants: &'a [ConstantBuffer],
}

pub trait PixelShader: Send + Sync {
    fn run(&self, input: &mut PSIn);
}

pub struct RenderTarget<'a> {
    pub width: u32,
    pub height: u32,
    buffer: &'a mut WindowBackBuffer<'a>,
}

impl<'a> RenderTarget<'a> {
    pub fn new(buffer: &'a mut WindowBackBuffer<'a>) -> Self {
        Self {
            width: buffer.window.width,
            height: buffer.window.height,
            buffer,
        }
    }

    pub fn clear(&mut self, color: Rgba8888UNORM) {
        self.buffer.clear(color);
    }

    pub fn get_buffer_mut(&mut self) -> &mut [u32] {
        self.buffer.as_slice_mut()
    }
}
