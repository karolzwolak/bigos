use core::simd::f32x4;

use micromath::F32Ext;

use crate::graphics::color::{Rgba8888F, Rgba8888UNORM};
use crate::graphics::pipeline::{
    PSIn, PixelShader, VSIn, VSOut, VSOut3D, Vertex3D, VertexShader, VertexShader3D,
};
use crate::graphics::resources::ConstantBuffer;
use crate::graphics::transform::{F32x4Ext, Matrix4x4};
use crate::serial_println;

pub struct FlatColorPS {
    pub color: Rgba8888UNORM,
}

impl PixelShader for FlatColorPS {
    fn run(&self, input: &mut PSIn) {
        input.render_target[0] = self.color.to_u32_xrgb();
    }
}

pub struct UVDebugPS;

impl PixelShader for UVDebugPS {
    fn run(&self, input: &mut PSIn) {
        let u = input.attributes[0];
        let v = input.attributes[1];
        let color = Rgba8888UNORM::from_rgbf32(u, v, 0f32);
        input.render_target[0] = color.to_u32_xrgb();
    }
}

pub struct TextureSamplePS {
    pub texture_slot: usize,
}

impl PixelShader for TextureSamplePS {
    fn run(&self, input: &mut PSIn) {
        if let Some(texture) = input.textures.get(self.texture_slot) {
            let u = input.attributes[0];
            let v = input.attributes[1];

            let nx = input.attributes[2];
            let ny = input.attributes[3];

            //let color = texture.sample_nearest(u, v);

            let color = Rgba8888UNORM::from_rgbf32(u, v, 0f32);

            // let color = Rgba8888UNORM::from_rgbf32(
            //     nx * 0.5f32 + 0.5f32,
            //     ny * 0.5f32 + 0.5f32,
            //     input.attributes[1] * 0.5f32 + 0.5f32,
            // );

            // serial_println!(
            //     "PS - color: {} {} {} to uv: {}, {}",
            //     color.r,
            //     color.g,
            //     color.b,
            //     u,
            //     v
            // );
            //let color = Rgba8888UNORM::GREEN;
            input.render_target[0] = color.to_u32_xrgb();
        }
    }
}

pub struct BlinnPhongPS {
    pub light_dir_intensity: f32x4,
    pub albedo: Rgba8888F,
    pub specular_color: Rgba8888F, // Specular color (usually white)
    pub shininess: f32,            // Specular power/exponent
    pub ambient_color: Rgba8888F,  // Ambient light color
    pub camera_pos: f32x4,         // Camera/eye position in world space
}

impl PixelShader for BlinnPhongPS {
    fn run(&self, input: &mut PSIn) {
        let normal = f32x4::from_array([
            input.attributes[2],
            input.attributes[3],
            input.extra[0],
            0.0,
        ])
        .normalize();

        let world_pos = f32x4::from_array([input.extra[1], input.extra[2], input.extra[3], 0.0]);

        let view_dir = (self.camera_pos - world_pos).normalize();

        let light_dir = -self.light_dir_intensity.normalize();
        let light_intensity = self.light_dir_intensity[3];

        let h = (light_dir + view_dir).normalize();
        let nol = normal.dot3(light_dir).max(0.0);
        let diffuse = nol * light_intensity;
        let noh = normal.dot3(h).max(0.0);
        let specular = if nol > 0.0 {
            noh.powf(self.shininess) * light_intensity
        } else {
            0.0
        };

        let ambient = self.ambient_color;

        let r = ambient.r + self.albedo.r * diffuse + self.specular_color.r * specular;
        let g = ambient.g + self.albedo.g * diffuse + self.specular_color.g * specular;
        let b = ambient.b + self.albedo.b * diffuse + self.specular_color.b * specular;

        let final_color = Rgba8888F::from_rgbf32(r, g, b);

        //let final_color = Rgba8888F::from_rgbf32(normal[0], normal[1], normal[2]);

        input.render_target[0] = final_color.to_u32_xrgb();
    }
}

pub struct PassThroughVS;

impl VertexShader for PassThroughVS {
    fn run(&self, input: &VSIn, output: &mut VSOut, _uniforms: &[ConstantBuffer]) {
        output.position = f32x4::from_array([
            f32::from_ne_bytes(input.vertex_data[0..4].try_into().unwrap()),
            f32::from_ne_bytes(input.vertex_data[4..8].try_into().unwrap()),
            0.0,
            1.0,
        ]);
    }
}

pub struct Basic3DVS {
    pub model_view_proj: Matrix4x4,
}

impl VertexShader3D for Basic3DVS {
    fn run(&self, input: &VSIn, output: &mut VSOut3D, _uniforms: &[ConstantBuffer]) {
        let vertex: &Vertex3D = unsafe { &*(input.vertex_data.as_ptr() as *const Vertex3D) };

        let pos = self.model_view_proj.mul_vec(vertex.pos);

        //serial_println!("pos {:?}", pos);

        output.position = pos;
        output.attributes = vertex.uv;
        output.extra = vertex.norm;
    }
}

pub struct BlinnPhongVS {
    pub model_view_proj: Matrix4x4,
    pub model_world: Matrix4x4,
}

impl VertexShader3D for BlinnPhongVS {
    fn run(&self, input: &VSIn, output: &mut VSOut3D, _uniforms: &[ConstantBuffer]) {
        let vertex: &Vertex3D = unsafe { &*(input.vertex_data.as_ptr() as *const Vertex3D) };

        let world_pos = self.model_world.mul_vec(vertex.pos);
        let world_normal = self.model_world.mul_vec(vertex.norm);

        let hom_pos = self.model_view_proj.mul_vec(vertex.pos);

        //serial_println!("pos {:?}", pos);

        output.position = hom_pos;
        output.world_position = world_pos;
        output.attributes = vertex.uv;
        output.extra = world_normal;
    }
}
