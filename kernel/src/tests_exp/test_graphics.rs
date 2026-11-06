use core::simd::f32x4;

use crate::graphics::color::Rgba8888F;
use crate::graphics::pipeline::{CullMode, DepthFunc, PipelineState3D, RenderTarget, Vertex3D};
use crate::graphics::resources::{ConstantBuffer, DepthBuffer};
use crate::graphics::shaders::{Basic3DVS, BlinnPhongPS, BlinnPhongVS, FlatColorPS, UVDebugPS};
use crate::graphics::transform::{Matrix4x4, create_perspective_matrix};
use crate::graphics::{
    color::Rgba8888UNORM,
    framebuffer::FrameBufferTarget,
    pipeline::{BlendState, PipelineState, RasterizerState, RenderMode, VertexLayout},
    renderer::RenderContext,
    resources::Texture,
    shaders::{PassThroughVS, TextureSamplePS},
    window::WindowBuffer,
};
use alloc::vec::Vec;
use alloc::{boxed::Box, sync::Arc};
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle};
use embedded_graphics::{geometry::Point, pixelcolor::Rgb888, primitives::Primitive};
use micromath::F32Ext;

pub fn draw_shapes(framebuffer_target: &mut FrameBufferTarget) {
    Rectangle::new(Point::new(0, 0), Size::new(100, 100))
        .into_styled(PrimitiveStyle::with_fill(Rgb888::RED))
        .draw(framebuffer_target)
        .unwrap();

    let style = PrimitiveStyleBuilder::new()
        .stroke_color(Rgb888::RED)
        .stroke_width(3)
        .fill_color(Rgb888::WHITE)
        .build();

    let fb_width = framebuffer_target.width as f32;
    let fb_height = framebuffer_target.height as f32;
    for i in 0..5 {
        let x = (fb_width / 9.0) * (i as f32 + 1.0) - 10.0;
        let y = (fb_height / 9.0) * (i as f32 + 1.0);
        let radius = 10.0 + i as f32 * 2.5;

        Circle::new(Point::new(x as i32, y as i32), radius as u32)
            .into_styled(style)
            .draw(framebuffer_target)
            .unwrap();
    }
}
