use crate::{
    graphics::{
        color::Rgba8888UNORM,
        framebuffer::FrameBufferTarget,
        pipeline::{
            BlendState, PipelineState, RasterizerState, RenderMode, Vertex2D, VertexLayout,
        },
        renderer::RenderContext,
        resources::Texture,
        shaders::{PassThroughVS, TextureSamplePS, UVDebugPS},
        texture::LOGO_TEXTURE_BYTES,
        window::WindowBuffer,
    },
    serial_println,
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

    let pipeline = PipelineState {
        vs: Box::new(PassThroughVS),
        ps: Box::new(UVDebugPS),
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
        ps: Box::new(UVDebugPS),
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

pub struct DynamicRenderer {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    uv_mode: bool,
    ctx: RenderContext,
    pipeline_logo: PipelineState,
    pipeline_uv: PipelineState,
}

impl DynamicRenderer {
    pub fn new() -> Self {
        const LOGO_WIDTH: u32 = 80;
        const LOGO_HEIGHT: u32 = 42;

        let mut ctx = RenderContext::new();

        let (chunks, remainder) = LOGO_TEXTURE_BYTES.as_chunks::<4>();
        debug_assert!(remainder.is_empty(), "Data length is not a multiple of 4");
        let texture_data: Vec<u32> = chunks
            .iter()
            .map(|chunk| u32::from_be_bytes(*chunk))
            .collect();
        let texture_slot =
            ctx.bind_texture(Texture::from_data(LOGO_WIDTH, LOGO_HEIGHT, texture_data));

        let pipeline_logo = PipelineState {
            vs: Box::new(PassThroughVS),
            ps: Box::new(TextureSamplePS { texture_slot }),
            vertex_layout: VertexLayout::new_2d(),
            rasterizer_state: RasterizerState::default(),
            blend_state: BlendState::default(),
            render_mode: RenderMode::XY,
        };

        let pipeline_uv = PipelineState {
            vs: Box::new(PassThroughVS),
            ps: Box::new(UVDebugPS),
            vertex_layout: VertexLayout::new_2d(),
            rasterizer_state: RasterizerState::default(),
            blend_state: BlendState::default(),
            render_mode: RenderMode::XY,
        };

        Self {
            x: 0.0,
            y: 0.0,
            vx: 0.25,
            vy: 0.25,
            uv_mode: false,
            ctx,
            pipeline_logo,
            pipeline_uv,
        }
    }

    pub fn setup(&mut self, uv_mode: bool) {
        self.uv_mode = uv_mode;
    }

    pub fn update(&mut self, window_buffer: &Arc<WindowBuffer>, dt_ms: f32) {
        const LOGO_WIDTH: u32 = 80;
        const LOGO_HEIGHT: u32 = 42;

        let max_x = window_buffer.width as f32 - LOGO_WIDTH as f32;
        let max_y = window_buffer.height as f32 - LOGO_HEIGHT as f32;

        self.x += self.vx * dt_ms;
        self.y += self.vy * dt_ms;

        if self.x <= 0.0 {
            self.x = 0.0;
            self.vx = self.vx.abs();
        }
        if self.x >= max_x {
            self.x = max_x;
            self.vx = -self.vx.abs();
        }
        if self.y <= 0.0 {
            self.y = 0.0;
            self.vy = self.vy.abs();
        }
        if self.y >= max_y {
            self.y = max_y;
            self.vy = -self.vy.abs();
        }

        let mut back_buffer = window_buffer.back_buffer_mut();
        let mut render_target = self.ctx.begin_frame(&mut back_buffer);
        render_target.clear(Rgba8888UNORM::WHITE);

        if self.uv_mode {
            self.ctx.draw_rect_2d(
                self.x,
                self.y,
                LOGO_WIDTH as f32,
                LOGO_HEIGHT as f32,
                &mut render_target,
                &self.pipeline_uv,
            );
        } else {
            self.ctx.draw_rect_2d(
                self.x,
                self.y,
                LOGO_WIDTH as f32,
                LOGO_HEIGHT as f32,
                &mut render_target,
                &self.pipeline_logo,
            );
        }

        window_buffer.present();
    }
}

impl Default for DynamicRenderer {
    fn default() -> Self {
        DynamicRenderer::new()
    }
}