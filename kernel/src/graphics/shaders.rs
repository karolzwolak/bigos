use core::simd::f32x4;

use crate::graphics::color::Rgba8888UNORM;
use crate::graphics::pipeline::{PSIn, PixelShader, VSIn, VSOut, VertexShader};
use crate::graphics::resources::ConstantBuffer;

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
            let color = texture.sample_nearest(u, v);

            input.render_target[0] = color.to_u32_xrgb();
        }
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
