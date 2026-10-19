//! Draw passes keyed by layer, registered from a map kind's render layers.

use mmc_core::{Layer, MapKindModule};

use crate::{DrawPass, HeightPass, PassInput, Renderer};

const LAND_SHADER: &str = "
@group(0) @binding(0) var<uniform> u: ViewUniform;
@group(0) @binding(1) var land: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let t = vec2i(floor(u.origin_px + (u.tile_offset + pos.xy) * u.scale));
    if any(t < u.lo) || any(t > u.hi) {
        return vec4f(0.0);
    }
    if textureLoad(land, t - u.tex_origin, 0).r > 0.5 {
        return vec4f(0.45, 0.62, 0.35, 1.0);
    }
    return vec4f(0.30, 0.50, 0.72, 1.0);
}
";

/// The land layer: flat land and sea colours.
pub struct LandPass(HeightPass);

impl LandPass {
    pub fn new(device: &wgpu::Device) -> LandPass {
        LandPass(HeightPass::with_shader(device, LAND_SHADER))
    }
}

impl DrawPass for LandPass {
    fn draw(&self, device: &wgpu::Device, input: &PassInput<'_>, pass: &mut wgpu::RenderPass<'_>) {
        self.0.draw(device, input, pass);
    }
}

impl Renderer {
    /// Registers the pass of each of `kind`'s render layers, bottom first.
    /// Layers with no pass in this crate are skipped.
    pub fn register_kind(&mut self, kind: &dyn MapKindModule) {
        for &layer in kind.render_layers() {
            let pass: Box<dyn DrawPass> = match layer {
                Layer::Height => Box::new(HeightPass::new(&self.device)),
                Layer::Land => Box::new(LandPass::new(&self.device)),
                _ => continue,
            };
            self.register(layer, pass);
        }
    }
}
