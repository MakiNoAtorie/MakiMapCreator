//! First draw pass of the height layer: hypsometric colours with hillshade.

use crate::{DrawPass, PassInput, TARGET_FORMAT, VIEW_WGSL};

const SHADER: &str = "
@group(0) @binding(0) var<uniform> u: ViewUniform;
@group(0) @binding(1) var heights: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
}

fn height(t: vec2i) -> f32 {
    return textureLoad(heights, clamp(t, u.lo, u.hi) - u.tex_origin, 0).r;
}

// (height in metres, colour); linear between stops.
const STOPS = array<vec4f, 9>(
    vec4f(-4000.0, 0.05, 0.10, 0.35),
    vec4f(-200.0, 0.15, 0.35, 0.65),
    // Sea level (0) is water.
    vec4f(0.0, 0.35, 0.60, 0.80),
    vec4f(0.001, 0.30, 0.55, 0.25),
    vec4f(300.0, 0.55, 0.70, 0.35),
    vec4f(1000.0, 0.75, 0.65, 0.40),
    vec4f(2500.0, 0.55, 0.45, 0.35),
    vec4f(4000.0, 0.95, 0.95, 0.95),
    vec4f(100000.0, 0.95, 0.95, 0.95),
);

fn ramp(h: f32) -> vec3f {
    if h <= STOPS[0].x {
        return STOPS[0].yzw;
    }
    for (var i = 1; i < 9; i++) {
        if h <= STOPS[i].x {
            let a = STOPS[i - 1];
            let b = STOPS[i];
            return mix(a.yzw, b.yzw, (h - a.x) / (b.x - a.x));
        }
    }
    return STOPS[8].yzw;
}

@fragment
fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let src = u.origin_px + (u.tile_offset + pos.xy) * u.scale;
    let t = vec2i(floor(src));
    if any(t < u.lo) || any(t > u.hi) {
        return vec4f(0.0);
    }
    let dx = (height(t + vec2i(1, 0)) - height(t - vec2i(1, 0))) / (2.0 * u.metres_per_pixel);
    let dy = (height(t + vec2i(0, 1)) - height(t - vec2i(0, 1))) / (2.0 * u.metres_per_pixel);
    let n = normalize(vec3f(-dx, -dy, 1.0));
    // Light from the north-west (image up-left), 45 degrees high. Flat ground keeps its ramp colour.
    let light = normalize(vec3f(-1.0, -1.0, 1.4142135));
    let shade = clamp(0.5 + 0.5 * dot(n, light) / light.z, 0.3, 1.3);
    return vec4f(clamp(ramp(height(t)) * shade, vec3f(0.0), vec3f(1.0)), 1.0);
}
";

pub struct HeightPass {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

impl HeightPass {
    pub fn new(device: &wgpu::Device) -> HeightPass {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("height"),
            source: wgpu::ShaderSource::Wgsl(format!("{VIEW_WGSL}{SHADER}").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("height"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("height"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("height"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TARGET_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        HeightPass { pipeline, layout }
    }
}

impl DrawPass for HeightPass {
    fn draw(&self, device: &wgpu::Device, input: &PassInput<'_>, pass: &mut wgpu::RenderPass<'_>) {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("height"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.view.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(input.layer),
                },
            ],
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
