use crate::graphics::color::Rgba8888UNORM;
use crate::graphics::pipeline::{PSIn, PipelineState, RenderTarget, VSOut, Vertex2D};
use crate::graphics::resources::{ConstantBuffer, RWBuffer, Texture};
use crate::graphics::window::WindowBackBuffer;
use alloc::vec::Vec;
use core::arch::x86_64::*;
use core::simd::{cmp::SimdPartialOrd, f32x4};

const MIN_TRIANGLE_AREA: f32 = 0.0001;
pub struct RenderContext {
    textures: Vec<Texture>,
    constant_buffers: Vec<ConstantBuffer>,
}

impl Default for RenderContext {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderContext {
    pub fn new() -> Self {
        Self {
            textures: Vec::new(),
            constant_buffers: Vec::new(),
        }
    }

    pub fn bind_texture(&mut self, texture: Texture) -> usize {
        let idx = self.textures.len();
        self.textures.push(texture);
        idx
    }

    pub fn bind_rwbuffer(&mut self, _buffer: RWBuffer) -> usize {
        0
    }

    pub fn bind_cbuffer(&mut self, constant_buffer: ConstantBuffer) -> usize {
        let idx = self.constant_buffers.len();
        self.constant_buffers.push(constant_buffer);
        idx
    }

    pub fn begin_frame<'a>(&self, backbuffer: &'a mut WindowBackBuffer<'a>) -> RenderTarget<'a> {
        RenderTarget::new(backbuffer)
    }

    pub fn transform_vertices(&self, vertices: &[Vertex2D; 4]) -> [VSOut; 4] {
        let positions = Vertex2D::load_four(vertices);

        let transformed = self.apply_matrix_to_vertices(&positions);

        transformed.map(|vert| VSOut::from_xyuv(&vert))
    }

    fn apply_matrix_to_vertices(&self, vertices: &[f32x4; 4]) -> [f32x4; 4] {
        *vertices
    }

    pub fn draw_triangle_2d(
        &mut self,
        v0: Vertex2D,
        v1: Vertex2D,
        v2: Vertex2D,
        render_target: &mut RenderTarget<'_>,
        pipeline: &PipelineState,
    ) {
        self.draw_single_triangle_vertex_list(&[v0, v1, v2], render_target, pipeline);
    }

    pub fn draw_rect_2d(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        render_target: &mut RenderTarget<'_>,
        pipeline: &PipelineState,
    ) {
        let vertices = [
            Vertex2D::new(x, y, 0.0, 0.0),
            Vertex2D::new(x + width, y, 1.0, 0.0),
            Vertex2D::new(x + width, y + height, 1.0, 1.0),
            Vertex2D::new(x, y, 0.0, 0.0),
            Vertex2D::new(x + width, y + height, 1.0, 1.0),
            Vertex2D::new(x, y + height, 0.0, 1.0),
        ];

        self.draw_triangle_pair_vertex_list(&vertices, render_target, pipeline);
    }

    fn draw_triangle_pair_vertex_list(
        &mut self,
        vertices: &[Vertex2D; 6],
        render_target: &mut RenderTarget<'_>,
        pipeline: &PipelineState,
    ) {
        let vs0 = VSOut::from_xyuv(&vertices[0].xyuv);
        let vs1 = VSOut::from_xyuv(&vertices[1].xyuv);
        let vs2 = VSOut::from_xyuv(&vertices[2].xyuv);
        let vs3 = VSOut::from_xyuv(&vertices[3].xyuv);
        let vs4 = VSOut::from_xyuv(&vertices[4].xyuv);
        let vs5 = VSOut::from_xyuv(&vertices[5].xyuv);

        self.rasterize_triangle_simd(&vs0, &vs1, &vs2, render_target, pipeline);
        self.rasterize_triangle_simd(&vs3, &vs4, &vs5, render_target, pipeline);
    }

    fn draw_single_triangle_vertex_list(
        &mut self,
        vertices: &[Vertex2D; 3],
        render_target: &mut RenderTarget<'_>,
        pipeline: &PipelineState,
    ) {
        let vs0 = VSOut::from_xyuv(&vertices[0].xyuv);
        let vs1 = VSOut::from_xyuv(&vertices[1].xyuv);
        let vs2 = VSOut::from_xyuv(&vertices[2].xyuv);

        self.rasterize_triangle_simd(&vs0, &vs1, &vs2, render_target, pipeline);
    }

    fn rasterize_triangle_simd(
        &mut self,
        v0: &VSOut,
        v1: &VSOut,
        v2: &VSOut,
        render_target: &mut RenderTarget<'_>,
        pipeline: &PipelineState,
    ) {
        let rt_width = render_target.width;
        let rt_height = render_target.height;
        let rt_buffer = render_target.get_buffer_mut();
        let x0 = f32x4::splat(v0.x());
        let y0 = f32x4::splat(v0.y());
        let x1 = f32x4::splat(v1.x());
        let y1 = f32x4::splat(v1.y());
        let x2 = f32x4::splat(v2.x());
        let y2 = f32x4::splat(v2.y());

        let e0_dx = x1 - x0;
        let e0_dy = y1 - y0;
        let e0_const = e0_dx * y0 - e0_dy * x0;
        let e1_dx = x2 - x1;
        let e1_dy = y2 - y1;
        let e1_const = e1_dx * y1 - e1_dy * x1;
        let e2_dx = x0 - x2;
        let e2_dy = y0 - y2;
        let e2_const = e2_dx * y2 - e2_dy * x2;

        // Compute triangle area
        let area = (v1.x() - v0.x()) * (v2.y() - v0.y()) - (v1.y() - v0.y()) * (v2.x() - v0.x());
        if area.abs() < MIN_TRIANGLE_AREA {
            return;
        }
        let inv_area = f32x4::splat(1.0 / area);

        // Compute bounding box
        let min_x = v0.x().min(v1.x()).min(v2.x()).max(0.0) as u32;
        let max_x = v0.x().max(v1.x()).max(v2.x()).min(rt_width as f32 - 1.0) as u32;
        let min_y = v0.y().min(v1.y()).min(v2.y()).max(0.0) as u32;
        let max_y = v0.y().max(v1.y()).max(v2.y()).min(rt_height as f32 - 1.0) as u32;

        // Process in 2x2 pixel quads
        // align to 2x2 pixel boundaries, round down
        let y_start = min_y & !1;
        let x_start = min_x & !1;

        for y in (y_start..=max_y).step_by(2) {
            for x in (x_start..=max_x).step_by(2) {
                // Create 4 pixel positions in a 2x2 quad
                let px = f32x4::from_array([
                    x as f32 + 0.5,
                    (x + 1) as f32 + 0.5,
                    x as f32 + 0.5,
                    (x + 1) as f32 + 0.5,
                ]);

                let py = f32x4::from_array([
                    y as f32 + 0.5,
                    y as f32 + 0.5,
                    (y + 1) as f32 + 0.5,
                    (y + 1) as f32 + 0.5,
                ]);

                let w0 = self.edge_function(e1_dx, e1_dy, px, py, e1_const);
                let w1 = self.edge_function(e2_dx, e2_dy, px, py, e2_const);
                let w2 = self.edge_function(e0_dx, e0_dy, px, py, e0_const);

                // Barycentric coordinates
                let w0 = w0 * inv_area;
                let w1 = w1 * inv_area;
                let w2 = w2 * inv_area;

                // Inside test for all 4 pixels
                let epsilon = f32x4::splat(0.0);
                let inside = w0.simd_ge(epsilon) & w1.simd_ge(epsilon) & w2.simd_ge(epsilon);
                let mask = inside.to_bitmask();
                if mask == 0 {
                    continue; // No pixels in this quad are inside
                }

                let interp_attrs = self.interpolate_attributes_barycentric(
                    &v0.attributes,
                    &v1.attributes,
                    &v2.attributes,
                    w0,
                    w1,
                    w2,
                );

                // Process each active pixel in the quad
                for i in 0..4 {
                    if (mask & (1 << i)) != 0 {
                        let screen_x = x + (i & 1) as u32;
                        let screen_y = y + ((i >> 1) as u32);

                        if screen_x <= max_x && screen_y <= max_y {
                            let idx = (screen_y * rt_width + screen_x) as usize;

                            let mut pixel_input = unsafe {
                                PSIn {
                                    attributes: interp_attrs,
                                    screen_x: screen_x as u16,
                                    screen_y: screen_y as u16,
                                    textures: &self.textures,
                                    render_target: core::slice::from_raw_parts_mut(
                                        rt_buffer.as_mut_ptr().add(idx),
                                        1,
                                    ),
                                    constants: &self.constant_buffers,
                                }
                            };

                            pipeline.ps.run(&mut pixel_input);
                        }
                    }
                }
            }
        }
    }

    // #[inline(always)]
    // fn edge_function(
    //     &self,
    //     ax: f32x4, ay: f32x4,
    //     bx: f32x4, by: f32x4,
    //     px: f32x4, py: f32x4,
    // ) -> f32x4 {
    //     (bx - ax) * (py - ay) - (by - ay) * (px - ax)
    // }

    #[inline(always)]
    fn edge_function(&self, dx: f32x4, dy: f32x4, px: f32x4, py: f32x4, constant: f32x4) -> f32x4 {
        dx * py - dy * px - constant
    }

    #[inline(always)]
    fn interpolate_attributes_barycentric(
        &self,
        attr0: &f32x4,
        attr1: &f32x4,
        attr2: &f32x4,
        w0: f32x4,
        w1: f32x4,
        w2: f32x4,
    ) -> f32x4 {
        (w0 * *attr0) + (w1 * *attr1) + (w2 * *attr2)
    }

    #[inline]
    pub fn clear(&self, render_target: &mut RenderTarget<'_>, color: Rgba8888UNORM) {
        unsafe {
            let buffer = render_target.get_buffer_mut();
            let len = buffer.len();
            let color_u32 = color.to_u32_xrgb();

            let color_vec = _mm_set1_epi32(color_u32 as i32);

            let mut i = 0;
            while i + 4 <= len {
                let ptr = buffer.as_mut_ptr().add(i) as *mut __m128i;
                _mm_storeu_si128(ptr, color_vec);
                i += 4;
            }
            for slot in buffer.iter_mut().take(len).skip(i) {
                *slot = color_u32;
            }
        }
    }
}
