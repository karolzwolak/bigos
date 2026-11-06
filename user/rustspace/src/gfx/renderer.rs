use crate::gfx::color::Rgba8888UNORM;
use crate::gfx::pipeline::{
    CullMode, PSIn, PipelineState, PipelineState3D, RenderMode, RenderTarget, VSIn, VSOut, VSOut3D,
    Vertex2D, Vertex3D,
};
use crate::gfx::resources::{ConstantBuffer, DepthBuffer, Texture};
use crate::gfx::surface::UserSurface;
use alloc::vec::Vec;
use core::simd::{cmp::SimdPartialOrd, f32x4, num::SimdFloat};

const MIN_TRIANGLE_AREA: f32 = 0.0001;
const IS_2D_Y_FLIPPED: bool = true;

pub struct RenderContext {
    textures: Vec<Texture>,
    constant_buffers: Vec<ConstantBuffer>,
}

#[inline(always)]
fn edge_function_scalar(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
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

    pub fn bind_cbuffer(&mut self, cbuf: ConstantBuffer) -> usize {
        let idx = self.constant_buffers.len();
        self.constant_buffers.push(cbuf);
        idx
    }

    pub fn begin_frame<'a>(&self, surface: &'a mut UserSurface) -> RenderTarget<'a> {
        RenderTarget::new(surface)
    }

    pub fn clear(&self, render_target: &mut RenderTarget<'_>, color: Rgba8888UNORM) {
        let buffer = render_target.get_buffer_mut();
        let color_u32 = color.to_u32_xrgb();
        // Plain fill — compiler will auto-vectorize this on x86_64 with SSE/AVX.
        for slot in buffer.iter_mut() {
            *slot = color_u32;
        }
    }

    fn process_vertex_3d(&self, vertex: &Vertex3D, pipeline: &PipelineState3D) -> VSOut3D {
        let vertex_bytes = unsafe {
            core::slice::from_raw_parts(
                vertex as *const Vertex3D as *const u8,
                core::mem::size_of::<Vertex3D>(),
            )
        };

        let input = VSIn {
            vertex_data: vertex_bytes,
            vertex_id: 0,
            instance_id: 0,
        };

        let mut output =
            VSOut3D::with_attributes(f32x4::splat(0.0), f32x4::splat(0.0), f32x4::splat(0.0));

        pipeline.vs.run(&input, &mut output, &self.constant_buffers);
        output
    }

    #[inline(always)]
    fn clip_to_screen(&self, v: &VSOut3D, rt_width: u32, rt_height: u32) -> (f32, f32, f32) {
        let w = v.w();
        let inv_w = if w.abs() > f32::EPSILON { 1.0 / w } else { 1.0 };
        let ndc_x = v.x() * inv_w;
        let ndc_y = v.y() * inv_w;
        let ndc_z = v.z() * inv_w;
        let screen_x = (ndc_x + 1.0) * 0.5 * rt_width as f32;
        let screen_y = (1.0 - ndc_y) * 0.5 * rt_height as f32;
        (screen_x, screen_y, ndc_z)
    }

    pub fn rasterize_triangle_3d(
        &mut self,
        v0: &VSOut3D,
        v1: &VSOut3D,
        v2: &VSOut3D,
        rt_buffer: &mut [u32],
        depth_buffer: &mut DepthBuffer,
        rt_width: u32,
        rt_height: u32,
        pipeline: &PipelineState3D,
    ) {
        let (sx0, sy0, z0) = self.clip_to_screen(v0, rt_width, rt_height);
        let (sx1, sy1, z1) = self.clip_to_screen(v1, rt_width, rt_height);
        let (sx2, sy2, z2) = self.clip_to_screen(v2, rt_width, rt_height);

        let edge = edge_function_scalar(sx0, sy0, sx1, sy1, sx2, sy2);
        if matches!(pipeline.rasterizer_state.cull_mode, CullMode::Back) && edge < 0.0 {
            return;
        }
        if matches!(pipeline.rasterizer_state.cull_mode, CullMode::Front) && edge >= 0.0 {
            return;
        }

        let area = edge;
        if area.abs() < MIN_TRIANGLE_AREA {
            return;
        }
        let inv_area = f32x4::splat(1.0 / area);

        let x0 = f32x4::splat(sx0);
        let y0 = f32x4::splat(sy0);
        let x1 = f32x4::splat(sx1);
        let y1 = f32x4::splat(sy1);
        let x2 = f32x4::splat(sx2);
        let y2 = f32x4::splat(sy2);

        let e0_dx = x1 - x0;
        let e0_dy = y1 - y0;
        let e0_const = e0_dx * y0 - e0_dy * x0;
        let e1_dx = x2 - x1;
        let e1_dy = y2 - y1;
        let e1_const = e1_dx * y1 - e1_dy * x1;
        let e2_dx = x0 - x2;
        let e2_dy = y0 - y2;
        let e2_const = e2_dx * y2 - e2_dy * x2;

        let min_x = sx0.min(sx1).min(sx2).max(0.0) as u32;
        let max_x = sx0.max(sx1).max(sx2).min(rt_width as f32 - 1.0) as u32;
        let min_y = sy0.min(sy1).min(sy2).max(0.0) as u32;
        let max_y = sy0.max(sy1).max(sy2).min(rt_height as f32 - 1.0) as u32;

        if min_x > max_x || min_y > max_y {
            return;
        }

        let w0_clip = v0.w();
        let w1_clip = v1.w();
        let w2_clip = v2.w();

        for y in (min_y & !1..=max_y).step_by(2) {
            for x in (min_x & !1..=max_x).step_by(2) {
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

                let w0 = self.edge_fn(e1_dx, e1_dy, px, py, e1_const);
                let w1 = self.edge_fn(e2_dx, e2_dy, px, py, e2_const);
                let w2 = self.edge_fn(e0_dx, e0_dy, px, py, e0_const);

                let alpha = w0 * inv_area;
                let beta = w1 * inv_area;
                let gamma = w2 * inv_area;

                let eps = f32x4::splat(0.0);
                let mask =
                    (alpha.simd_ge(eps) & beta.simd_ge(eps) & gamma.simd_ge(eps)).to_bitmask();
                if mask == 0 {
                    continue;
                }

                let a = alpha * f32x4::splat(1.0 / w0_clip);
                let b = beta * f32x4::splat(1.0 / w1_clip);
                let g = gamma * f32x4::splat(1.0 / w2_clip);
                let inv_denom = f32x4::splat(1.0) / (a + b + g);

                let z_interp = (f32x4::splat(z0) * a + f32x4::splat(z1) * b + f32x4::splat(z2) * g)
                    * inv_denom;

                for i in 0..4 {
                    if (mask & (1 << i)) != 0 {
                        let screen_x = x + (i & 1) as u32;
                        let screen_y = y + (i >> 1) as u32;
                        if screen_x <= max_x && screen_y <= max_y {
                            if depth_buffer.test_and_set(screen_x, screen_y, z_interp[i]) {
                                let idx = (screen_y * rt_width + screen_x) as usize;
                                let mut ps_in = unsafe {
                                    PSIn {
                                        attributes: f32x4::splat(0.0),
                                        extra: f32x4::splat(0.0),
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
                                pipeline.ps.run(&mut ps_in);
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn draw_indexed_3d(
        &mut self,
        vertices: &[Vertex3D],
        indices: &[u32],
        render_target: &mut RenderTarget<'_>,
        depth_buffer: &mut DepthBuffer,
        pipeline: &PipelineState3D,
    ) {
        debug_assert_eq!(pipeline.render_mode, RenderMode::XYZ);

        let rt_width = render_target.width;
        let rt_height = render_target.height;
        let rt_buffer = render_target.get_buffer_mut();

        let (chunks, _) = indices.as_chunks::<3>();
        for tri in chunks {
            let v0 = &vertices[tri[0] as usize];
            let v1 = &vertices[tri[1] as usize];
            let v2 = &vertices[tri[2] as usize];

            let vs0 = self.process_vertex_3d(v0, pipeline);
            let vs1 = self.process_vertex_3d(v1, pipeline);
            let vs2 = self.process_vertex_3d(v2, pipeline);

            if vs0.w() <= 0.0 && vs1.w() <= 0.0 && vs2.w() <= 0.0 {
                continue;
            }

            self.rasterize_triangle_3d(
                &vs0,
                &vs1,
                &vs2,
                rt_buffer,
                depth_buffer,
                rt_width,
                rt_height,
                pipeline,
            );
        }
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
        let vertices = if IS_2D_Y_FLIPPED {
            let fy = y + height;
            let fyh = y;
            [
                Vertex2D::new(x, fy, 0.0, 0.0),
                Vertex2D::new(x + width, fyh, 1.0, 1.0),
                Vertex2D::new(x + width, fy, 1.0, 0.0),
                Vertex2D::new(x, fy, 0.0, 0.0),
                Vertex2D::new(x, fyh, 0.0, 1.0),
                Vertex2D::new(x + width, fyh, 1.0, 1.0),
            ]
        } else {
            [
                Vertex2D::new(x, y, 0.0, 0.0),
                Vertex2D::new(x + width, y + height, 1.0, 1.0),
                Vertex2D::new(x + width, y, 1.0, 0.0),
                Vertex2D::new(x, y, 0.0, 0.0),
                Vertex2D::new(x, y + height, 0.0, 1.0),
                Vertex2D::new(x + width, y + height, 1.0, 1.0),
            ]
        };

        let rt_width = render_target.width;
        let rt_height = render_target.height;
        let rt_buffer = render_target.get_buffer_mut();

        let vs = vertices.map(|v| VSOut::from_xyuv(&v.xyuv));
        self.rasterize_triangle_simd(
            &vs[0], &vs[1], &vs[2], rt_buffer, rt_width, rt_height, pipeline,
        );
        self.rasterize_triangle_simd(
            &vs[3], &vs[4], &vs[5], rt_buffer, rt_width, rt_height, pipeline,
        );
    }

    pub fn draw_triangle_2d(
        &mut self,
        x0: f32,
        y0: f32,
        u0: f32,
        v0: f32,
        x1: f32,
        y1: f32,
        u1: f32,
        v1: f32,
        x2: f32,
        y2: f32,
        u2: f32,
        v2: f32,
        render_target: &mut RenderTarget<'_>,
        pipeline: &PipelineState,
    ) {
        let vertices = [
            Vertex2D::new(x0, y0, u0, v0),
            Vertex2D::new(x1, y1, u1, v1),
            Vertex2D::new(x2, y2, u2, v2),
        ];

        let rt_width = render_target.width;
        let rt_height = render_target.height;
        let rt_buffer = render_target.get_buffer_mut();

        let vs0 = VSOut::from_xyuv(&vertices[0].xyuv);
        let vs1 = VSOut::from_xyuv(&vertices[1].xyuv);
        let vs2 = VSOut::from_xyuv(&vertices[2].xyuv);
        self.rasterize_triangle_simd(&vs0, &vs1, &vs2, rt_buffer, rt_width, rt_height, pipeline);
    }

    fn rasterize_triangle_simd(
        &mut self,
        v0: &VSOut,
        v1: &VSOut,
        v2: &VSOut,
        rt_buffer: &mut [u32],
        rt_width: u32,
        rt_height: u32,
        pipeline: &PipelineState,
    ) {
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

        let area = if IS_2D_Y_FLIPPED {
            edge_function_scalar(x0[0], y0[0], x1[0], y1[0], x2[0], y2[0])
        } else {
            (v1.x() - v0.x()) * (v2.y() - v0.y()) - (v1.y() - v0.y()) * (v2.x() - v0.x())
        };
        if area.abs() < MIN_TRIANGLE_AREA {
            return;
        }
        let inv_area = f32x4::splat(1.0 / area);

        let min_x = v0.x().min(v1.x()).min(v2.x()).max(0.0) as u32;
        let max_x = v0.x().max(v1.x()).max(v2.x()).min(rt_width as f32 - 1.0) as u32;
        let min_y = v0.y().min(v1.y()).min(v2.y()).max(0.0) as u32;
        let max_y = v0.y().max(v1.y()).max(v2.y()).min(rt_height as f32 - 1.0) as u32;

        for y in (min_y & !1..=max_y).step_by(2) {
            for x in (min_x & !1..=max_x).step_by(2) {
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

                let (w0, w1, w2) = if IS_2D_Y_FLIPPED {
                    (
                        self.edge_fn(e1_dx, e1_dy, px, py, e1_const),
                        self.edge_fn(e2_dx, e2_dy, px, py, e2_const),
                        self.edge_fn(e0_dx, e0_dy, px, py, e0_const),
                    )
                } else {
                    (
                        self.edge_fn_flip(e1_dx, e1_dy, px, py, e1_const),
                        self.edge_fn_flip(e2_dx, e2_dy, px, py, e2_const),
                        self.edge_fn_flip(e0_dx, e0_dy, px, py, e0_const),
                    )
                };

                let bw0 = w0 * inv_area;
                let bw1 = w1 * inv_area;
                let bw2 = w2 * inv_area;

                let eps = f32x4::splat(0.0);
                let mask = (bw0.simd_ge(eps) & bw1.simd_ge(eps) & bw2.simd_ge(eps)).to_bitmask();
                if mask == 0 {
                    continue;
                }

                let mut interp = self.interpolate_bary(
                    &v0.attributes,
                    &v1.attributes,
                    &v2.attributes,
                    bw0,
                    bw1,
                    bw2,
                );
                if IS_2D_Y_FLIPPED {
                    interp[1] = 1.0 - interp[1];
                }

                for i in 0..4 {
                    if (mask & (1 << i)) != 0 {
                        let screen_x = x + (i & 1) as u32;
                        let screen_y = y + (i >> 1) as u32;
                        if screen_x <= max_x && screen_y <= max_y {
                            let idx = (screen_y * rt_width + screen_x) as usize;
                            let mut ps_in = unsafe {
                                PSIn {
                                    attributes: interp,
                                    extra: f32x4::splat(0.0),
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
                            pipeline.ps.run(&mut ps_in);
                        }
                    }
                }
            }
        }
    }

    #[inline(always)]
    fn edge_fn(&self, dx: f32x4, dy: f32x4, px: f32x4, py: f32x4, c: f32x4) -> f32x4 {
        dy * px - dx * py + c
    }

    #[inline(always)]
    fn edge_fn_flip(&self, dx: f32x4, dy: f32x4, px: f32x4, py: f32x4, c: f32x4) -> f32x4 {
        dx * py - dy * px - c
    }

    #[inline(always)]
    fn interpolate_bary(
        &self,
        a0: &f32x4,
        a1: &f32x4,
        a2: &f32x4,
        w0: f32x4,
        w1: f32x4,
        w2: f32x4,
    ) -> f32x4 {
        (w0 * *a0) + (w1 * *a1) + (w2 * *a2)
    }
}
