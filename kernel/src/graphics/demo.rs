use crate::graphics::{
    color::Rgba8888UNORM,
    framebuffer::FrameBufferTarget,
    pipeline::{BlendState, PipelineState, RasterizerState, RenderMode, Vertex2D, VertexLayout},
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

pub fn init_demo_filesystem() {
    use crate::filesystem::{fat32::test_data::create_fat32_image, init_filesystem};
    let image = create_fat32_image();
    init_filesystem(&*image).expect("filesystem init failed");
}

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

pub fn render_shaders(window_buffer: &Arc<WindowBuffer>) {
    let mut ctx = RenderContext::new();
    const SIZE: u32 = 120;
    let texture_data = (0..(SIZE * SIZE))
        .map(|i| {
            let y = i;
            if y % 2 == 0 {
                Rgba8888UNORM::from_rgb(255, 0, 0).to_u32_rgba()
            } else {
                Rgba8888UNORM::from_rgb(0, 255, 0).to_u32_rgba()
            }
        })
        .collect::<Vec<u32>>();

    let texture = Texture::from_data(SIZE, SIZE, texture_data);
    let texture_slot = ctx.bind_texture(texture);

    let texture_data = (0..(SIZE * SIZE))
        .map(|i| {
            let _y = i;
            Rgba8888UNORM::from_rgb(0, 255, 255).to_u32_rgba()
        })
        .collect::<Vec<u32>>();
    let texture = Texture::from_data(SIZE, SIZE, texture_data);
    let texture_slot2 = ctx.bind_texture(texture);

    let pipeline = PipelineState {
        vs: Box::new(PassThroughVS),
        ps: Box::new(TextureSamplePS { texture_slot }),
        vertex_layout: VertexLayout::new_2d(),
        rasterizer_state: RasterizerState::default(),
        blend_state: BlendState::default(),
        render_mode: RenderMode::XY,
    };

    let mut back_buffer = window_buffer.back_buffer_mut();
    let mut render_target = ctx.begin_frame(&mut back_buffer);
    render_target.clear(Rgba8888UNORM::GRAY);

    ctx.draw_rect_2d(
        10.0,
        10.0,
        SIZE as f32,
        SIZE as f32,
        &mut render_target,
        &pipeline,
    );

    let pipeline2 = PipelineState {
        vs: Box::new(PassThroughVS),
        ps: Box::new(TextureSamplePS {
            texture_slot: texture_slot2,
        }),
        vertex_layout: VertexLayout::new_2d(),
        rasterizer_state: RasterizerState::default(),
        blend_state: BlendState::default(),
        render_mode: RenderMode::XY,
    };
    ctx.draw_rect_2d(
        25.0,
        25.0,
        SIZE as f32,
        SIZE as f32,
        &mut render_target,
        &pipeline2,
    );

    ctx.draw_triangle_2d(
        Vertex2D::new(20.0 + 40.0, 50.0 + 40.0, 0.0, 0.0),
        Vertex2D::new(50.0 + 40.0, 0.0 + 40.0, 1.0, 0.0),
        Vertex2D::new(80.0 + 40.0, 50.0 + 40.0, 0.5, 1.0),
        &mut render_target,
        &pipeline,
    );

    window_buffer.present();
}
