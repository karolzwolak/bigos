use crate::graphics::color::Rgba8888UNORM;
use crate::graphics::pipeline::{
    CullMode, PSIn, PipelineState, PipelineState3D, PixelShader, RenderMode, RenderTarget, VSIn,
    VSOut, VSOut3D, Vertex2D, Vertex3D, VertexShader,
};
use crate::graphics::resources::{ConstantBuffer, DepthBuffer, RWBuffer, Texture};
use crate::graphics::window::{WindowBackBuffer, WindowBuffer};
use crate::serial_println;
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::x86_64::*;
use core::ops::{Add, Mul, Sub};
use core::simd::{Simd, cmp::SimdPartialOrd, f32x4, num::SimdFloat};

const MIN_TRIANGLE_AREA: f32 = 0.0001;
const IS_2D_Y_FLIPPED: bool = true;

const LIGHT_3D_PIPE: bool = true;

pub struct RenderContext {
    vertex_outputs: Vec<VSOut>,

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
            vertex_outputs: Vec::new(),
            textures: Vec::new(),
            constant_buffers: Vec::new(),
        }
    }

    pub fn bind_texture(&mut self, texture: Texture) -> usize {
        let idx = self.textures.len();
        self.textures.push(texture);
        idx
    }

    pub fn bind_rwbuffer(&mut self, buffer: RWBuffer) -> usize {
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
        let result = *vertices;
        result
    }

    pub fn process_vertex_3d(&self, vertex: &Vertex3D, pipeline: &PipelineState3D) -> VSOut3D {
        //TODO: optimize
        // SAFETY: Vertex3D is #[repr(C, align(16))] with no padding.
        // We're creating a byte slice view into the vertex's memory.
        let vertex_bytes = unsafe {
            core::slice::from_raw_parts(
                vertex as *const Vertex3D as *const u8,
                core::mem::size_of::<Vertex3D>(),
            )
        };

        let mut input = VSIn {
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
        // Perspective division
        let w = v.w();
        let inv_w = if w.abs() > f32::EPSILON { 1.0 / w } else { 1.0 };

        let ndc_x = v.x() * inv_w;
        let ndc_y = v.y() * inv_w;
        let ndc_z = v.z() * inv_w;

        // NDC to screen space
        let screen_x = (ndc_x + 1.0) * 0.5 * rt_width as f32;
        let screen_y = (1.0 - ndc_y) * 0.5 * rt_height as f32; // Flip Y
        let screen_z = ndc_z;

        (screen_x, screen_y, screen_z)
    }

    #[inline(always)]
    fn interpolate_perspective_correct(
        &self,
        attr0: &f32x4,
        attr1: &f32x4,
        attr2: &f32x4,
        w0: f32x4,
        w1: f32x4,
        w2: f32x4,
        inv_w0: f32,
        inv_w1: f32,
        inv_w2: f32,
    ) -> (f32x4, f32x4) {
        // Perspective-correct interpolation
        // corrected_attr = (w0*attr0/w0_orig + w1*attr1/w1_orig + w2*attr2/w2_orig) / (w0/w0_orig + w1/w1_orig + w2/w2_orig)

        let w0_rcp = f32x4::splat(inv_w0);
        let w1_rcp = f32x4::splat(inv_w1);
        let w2_rcp = f32x4::splat(inv_w2);

        let attr_num = (w0 * *attr0 * w0_rcp) + (w1 * *attr1 * w1_rcp) + (w2 * *attr2 * w2_rcp);
        let denom = (w0 * w0_rcp) + (w1 * w1_rcp) + (w2 * w2_rcp);

        // Return (interpolated_attributes, z_reciprocal_for_depth)
        let depth_z = f32x4::splat(1.0) / denom;
        let interp_attrs = attr_num * depth_z;

        (interp_attrs, depth_z)
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
        if matches!(pipeline.rasterizer_state.cull_mode, CullMode::Back) {
            if edge < 0.0 {
                return;
            }
        } else if matches!(pipeline.rasterizer_state.cull_mode, CullMode::Front) {
            if edge >= 0.0 {
                return;
            }
        }

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

        let area = edge_function_scalar(sx0, sy0, sx1, sy1, sx2, sy2);
        if area.abs() < MIN_TRIANGLE_AREA {
            return;
        }
        let inv_area = f32x4::splat(1.0 / area);

        // Compute bounding box
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

        // Process in 2x2 pixel quads
        let y_start = min_y & !1;
        let x_start = min_x & !1;

        for y in (y_start..=max_y).step_by(2) {
            for x in (x_start..=max_x).step_by(2) {
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

                // Scale to barycentric coordinates
                let alpha = w0 * inv_area;
                let beta = w1 * inv_area;
                let gamma = w2 * inv_area;

                // Inside test
                let epsilon = f32x4::splat(0.0);
                let inside =
                    alpha.simd_ge(epsilon) & beta.simd_ge(epsilon) & gamma.simd_ge(epsilon);
                let mask = inside.to_bitmask();
                if mask == 0 {
                    continue;
                }

                let rcp_w0 = f32x4::splat(1.0 / w0_clip);
                let rcp_w1 = f32x4::splat(1.0 / w1_clip);
                let rcp_w2 = f32x4::splat(1.0 / w2_clip);

                let a = alpha * rcp_w0;
                let b = beta * rcp_w1;
                let g = gamma * rcp_w2;

                let denom = a + b + g;
                let inv_denom = f32x4::splat(1.0) / denom;

                // let u0 = f32x4::splat(v0.attributes[0]);
                // let u1 = f32x4::splat(v1.attributes[0]);
                // let u2 = f32x4::splat(v2.attributes[0]);
                // let u_interp = (a * u0 + b * u1 + g * u2) * inv_denom;

                // let v0_val = f32x4::splat(v0.attributes[1]);
                // let v1_val = f32x4::splat(v1.attributes[1]);
                // let v2_val = f32x4::splat(v2.attributes[1]);
                // let v_interp = (a * v0_val + b * v1_val + g * v2_val) * inv_denom;

                // let nx0 = f32x4::splat(v0.extra[0]);
                // let nx1 = f32x4::splat(v1.extra[0]);
                // let nx2 = f32x4::splat(v2.extra[0]);
                // let nx_interp = (a * nx0 + b * nx1 + g * nx2) * inv_denom;

                // let ny0 = f32x4::splat(v0.extra[1]);
                // let ny1 = f32x4::splat(v1.extra[1]);
                // let ny2 = f32x4::splat(v2.extra[1]);
                // let ny_interp = (a * ny0 + b * ny1 + g * ny2) * inv_denom;

                // let nz0 = f32x4::splat(v0.extra[2]);
                // let nz1 = f32x4::splat(v1.extra[2]);
                // let nz2 = f32x4::splat(v2.extra[2]);
                // let nz_interp = (a * nz0 + b * nz1 + g * nz2) * inv_denom;

                // let world_x0 = f32x4::splat(v0.world_position[0]);
                // let world_x1 = f32x4::splat(v1.world_position[0]);
                // let world_x2 = f32x4::splat(v2.world_position[0]);
                // let world_x_interp = (a * world_x0 + b * world_x1 + g * world_x2) * inv_denom;

                // let world_y0 = f32x4::splat(v0.world_position[1]);
                // let world_y1 = f32x4::splat(v1.world_position[1]);
                // let world_y2 = f32x4::splat(v2.world_position[1]);
                // let world_y_interp = (a * world_y0 + b * world_y1 + g * world_y2) * inv_denom;

                // let world_z0 = f32x4::splat(v0.world_position[2]);
                // let world_z1 = f32x4::splat(v1.world_position[2]);
                // let world_z2 = f32x4::splat(v2.world_position[2]);
                // let world_z_interp = (a * world_z0 + b * world_z1 + g * world_z2) * inv_denom;

                //TODO: may be wrong
                let z_interp = (f32x4::splat(z0) * a + f32x4::splat(z1) * b + f32x4::splat(z2) * g)
                    * inv_denom;

                for i in 0..4 {
                    if (mask & (1 << i)) != 0 {
                        let screen_x = x + (i & 1) as u32;
                        let screen_y = y + ((i >> 1) as u32);

                        if screen_x <= max_x && screen_y <= max_y {
                            if depth_buffer.test_and_set(screen_x, screen_y, z_interp[i]) {
                                let idx = (screen_y * rt_width + screen_x) as usize;

                                let pixel_attrs = f32x4::from_array([
                                    //u_interp[i],
                                    //v_interp[i],
                                    //nx_interp[i],
                                    //ny_interp[i],
                                    0.0, 0.0, 0.0, 0.0,
                                ]);

                                let pixel_attrs_extra = f32x4::from_array([
                                    //nz_interp[i],
                                    //world_x_interp[i],
                                    //world_y_interp[i],
                                    //world_z_interp[i],
                                    0.0, 0.0, 0.0, 0.0,
                                ]);

                                let mut pixel_input = unsafe {
                                    PSIn {
                                        attributes: pixel_attrs,
                                        extra: pixel_attrs_extra,
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

        let (indices, _remainder) = indices.as_chunks::<3>();
        for triangle_indices in indices {
            if triangle_indices.len() < 3 {
                break;
            }

            let v0 = &vertices[triangle_indices[0] as usize];
            let v1 = &vertices[triangle_indices[1] as usize];
            let v2 = &vertices[triangle_indices[2] as usize];

            let vs0 = self.process_vertex_3d(v0, pipeline);
            let vs1 = self.process_vertex_3d(v1, pipeline);
            let vs2 = self.process_vertex_3d(v2, pipeline);

            if self.should_cull_triangle(&vs0, &vs1, &vs2) {
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

    fn should_cull_triangle(&self, v0: &VSOut3D, v1: &VSOut3D, v2: &VSOut3D) -> bool {
        // Cull if all vertices are behind the camera (w <= 0)
        if v0.w() <= 0.0 && v1.w() <= 0.0 && v2.w() <= 0.0 {
            return true;
        }

        //TODO:
        false
    }

    pub fn draw_triangle_2d<VS, PS>(
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
        pipeline: &PipelineState<VS, PS>,
    ) where
        VS: VertexShader,
        PS: PixelShader,
    {
        let vertices = [
            Vertex2D::new(x0, y0, u0, v0),
            Vertex2D::new(x1, y1, u1, v1),
            Vertex2D::new(x2, y2, u2, v2),
        ];

        self.draw_single_triangle_vertex_list(&vertices, render_target, pipeline);
    }

    pub fn draw_rect_2d<VS, PS>(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        render_target: &mut RenderTarget<'_>,
        pipeline: &PipelineState<VS, PS>,
    ) where
        VS: VertexShader,
        PS: PixelShader,
    {
        // let vertices = [
        //     Vertex2D::new(x, y, 0.0, 0.0),
        //     Vertex2D::new(x + width, y + height, 1.0, 1.0),
        //     Vertex2D::new(x + width, y, 1.0, 0.0),
        //     Vertex2D::new(x, y, 0.0, 0.0),
        //     Vertex2D::new(x, y + height, 0.0, 1.0),
        //     Vertex2D::new(x + width, y + height, 1.0, 1.0),
        // ];
        let vertices = if IS_2D_Y_FLIPPED {
            let fy = y + height; // Flip Y for screen space
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

        self.draw_triangle_pair_vertex_list(&vertices, render_target, pipeline);
    }

    fn draw_triangle_pair_vertex_list<VS, PS>(
        &mut self,
        vertices: &[Vertex2D; 6],
        render_target: &mut RenderTarget<'_>,
        pipeline: &PipelineState<VS, PS>,
    ) where
        VS: VertexShader,
        PS: PixelShader,
    {
        let rt_width = render_target.width;
        let rt_height = render_target.height;
        let rt_buffer = render_target.get_buffer_mut();

        let vs0 = VSOut::from_xyuv(&vertices[0].xyuv);
        let vs1 = VSOut::from_xyuv(&vertices[1].xyuv);
        let vs2 = VSOut::from_xyuv(&vertices[2].xyuv);
        let vs3 = VSOut::from_xyuv(&vertices[3].xyuv);
        let vs4 = VSOut::from_xyuv(&vertices[4].xyuv);
        let vs5 = VSOut::from_xyuv(&vertices[5].xyuv);

        self.rasterize_triangle_simd(&vs0, &vs1, &vs2, rt_buffer, rt_width, rt_height, pipeline);
        self.rasterize_triangle_simd(&vs3, &vs4, &vs5, rt_buffer, rt_width, rt_height, pipeline);
    }

    fn draw_single_triangle_vertex_list<VS, PS>(
        &mut self,
        vertices: &[Vertex2D; 3],
        render_target: &mut RenderTarget<'_>,
        pipeline: &PipelineState<VS, PS>,
    ) where
        VS: VertexShader,
        PS: PixelShader,
    {
        let rt_width = render_target.width;
        let rt_height = render_target.height;
        let rt_buffer = render_target.get_buffer_mut();

        let vs0 = VSOut::from_xyuv(&vertices[0].xyuv);
        let vs1 = VSOut::from_xyuv(&vertices[1].xyuv);
        let vs2 = VSOut::from_xyuv(&vertices[2].xyuv);

        self.rasterize_triangle_simd(&vs0, &vs1, &vs2, rt_buffer, rt_width, rt_height, pipeline);
    }

    fn draw_triangle_list<VS, PS>(
        &mut self,
        vertices: &[Vertex2D],
        render_target: &mut RenderTarget<'_>,
        pipeline: &PipelineState<VS, PS>,
    ) where
        VS: VertexShader,
        PS: PixelShader,
    {
        let rt_width = render_target.width;
        let rt_height = render_target.height;
        let rt_buffer = render_target.get_buffer_mut();

        let (chunks, _remainder) = vertices.as_chunks::<3>();
        for chunk in chunks {
            let v0 = &chunk[0];
            let v1 = &chunk[1];
            let v2 = &chunk[2];

            let vs0 = VSOut::from_xyuv(&v0.xyuv);
            let vs1 = VSOut::from_xyuv(&v1.xyuv);
            let vs2 = VSOut::from_xyuv(&v2.xyuv);

            self.rasterize_triangle_simd(
                &vs0, &vs1, &vs2, rt_buffer, rt_width, rt_height, pipeline,
            );
        }
    }

    fn rasterize_triangle_simd<VS, PS>(
        &mut self,
        v0: &VSOut,
        v1: &VSOut,
        v2: &VSOut,
        rt_buffer: &mut [u32],
        rt_width: u32,
        rt_height: u32,
        pipeline: &PipelineState<VS, PS>,
    ) where
        VS: VertexShader,
        PS: PixelShader,
    {
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
        let area = if IS_2D_Y_FLIPPED {
            edge_function_scalar(x0[0], y0[0], x1[0], y1[0], x2[0], y2[0])
        } else {
            (v1.x() - v0.x()) * (v2.y() - v0.y()) - (v1.y() - v0.y()) * (v2.x() - v0.x())
        };
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

                let (w0, w1, w2) = if IS_2D_Y_FLIPPED {
                    let w0 = self.edge_function(e1_dx, e1_dy, px, py, e1_const);
                    let w1 = self.edge_function(e2_dx, e2_dy, px, py, e2_const);
                    let w2 = self.edge_function(e0_dx, e0_dy, px, py, e0_const);
                    (w0, w1, w2)
                } else {
                    let w0 = self.edge_function_flip(e1_dx, e1_dy, px, py, e1_const);
                    let w1 = self.edge_function_flip(e2_dx, e2_dy, px, py, e2_const);
                    let w2 = self.edge_function_flip(e0_dx, e0_dy, px, py, e0_const);
                    (w0, w1, w2)
                };

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

                let mut interp_attrs = self.interpolate_attributes_barycentric(
                    &v0.attributes,
                    &v1.attributes,
                    &v2.attributes,
                    w0,
                    w1,
                    w2,
                );
                if IS_2D_Y_FLIPPED {
                    interp_attrs[1] = 1.0 - interp_attrs[1];
                }

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
        //dx * py - dy * px - constant
        dy * px - dx * py + constant
    }

    #[inline(always)]
    fn edge_function_flip(
        &self,
        dx: f32x4,
        dy: f32x4,
        px: f32x4,
        py: f32x4,
        constant: f32x4,
    ) -> f32x4 {
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
            for j in i..len {
                buffer[j] = color_u32;
            }
        }
    }
}
