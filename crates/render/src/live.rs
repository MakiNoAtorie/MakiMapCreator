//! Live brush (ADR-0025): a GPU copy of the drawn heights that stamps change in the frame they
//! are made. Display only; the CPU stroke stays the source of truth.

use mmc_core::{BrushMode, BrushStroke, Layer, MapId};
use wgpu::util::DeviceExt;

use crate::{Image, Result, Scene, ShaderPass, TARGET_FORMAT, View, read_texture};

/// What a copy covers: level pixels `x0..x0+w`, `y0..y0+h` of `level` of `map`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Area {
    map: MapId,
    level: u32,
    /// Level-0 metres per pixel.
    mpp: f64,
    /// Global level-0 pixel of map pixel (0, 0).
    origin: (i64, i64),
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
}

impl Area {
    /// Global level-0 pixel sampled for level pixel `x` (`y`): the one right of its centre.
    fn sample(&self, x: u32, y: u32) -> (i64, i64) {
        let half = (1i64 << self.level) / 2;
        (
            self.origin.0 + ((x as i64) << self.level) + half,
            self.origin.1 + ((y as i64) << self.level) + half,
        )
    }

    /// First pixel of the frame the shader works in: the first detail node, a multiple of 4.
    fn frame(&self) -> (i64, i64) {
        let (x, y) = self.sample(self.x0, self.y0);
        (x.div_euclid(4) * 4, y.div_euclid(4) * 4)
    }
}

/// One layer of a scene at its drawn level: what a live copy starts from.
pub struct LiveBase {
    area: Area,
    values: Vec<f32>,
}

impl LiveBase {
    pub fn map(&self) -> MapId {
        self.area.map
    }

    /// Level pixels row by row.
    pub fn values(&self) -> &[f32] {
        &self.values
    }
}

impl Scene {
    /// The scene's `layer` as the base of a live copy.
    pub fn into_base(self, layer: Layer) -> Option<LiveBase> {
        let mpp = self.metres_per_pixel / (1u64 << self.level) as f64;
        let (_, r) = self.rasters.into_iter().find(|(l, _)| *l == layer)?;
        Some(LiveBase {
            area: Area {
                map: self.view.map,
                level: self.level,
                mpp,
                origin: self.origin,
                x0: r.x0,
                y0: r.y0,
                w: r.w,
                h: r.h,
            },
            values: r.values,
        })
    }
}

struct Copy {
    area: Area,
    texture: wgpu::Texture,
    shade: wgpu::BindGroup,
}

/// Detail tables for (seed, scale bits) over the copy's area, as `live.wgsl` reads them.
struct Tables {
    key: (u64, u64),
    buffer: wgpu::Buffer,
    octaves: u32,
    total: f32,
}

/// The live copy: stamps run in a compute pass (`live.wgsl`), shading is the kind's height pass.
pub struct Live {
    device: wgpu::Device,
    queue: wgpu::Queue,
    stamp: wgpu::ComputePipeline,
    shading: ShaderPass,
    view: wgpu::Buffer,
    copy: Option<Copy>,
    tables: Option<Tables>,
}

/// What a frame draws from the copy, for a render pass recorded later (an egui callback).
#[derive(Clone)]
pub struct LiveFrame {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
}

impl LiveFrame {
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

const STAMP_BYTES: usize = 32;

impl Live {
    /// `shader` is the kind's height pass; `format` the target the copy is drawn into.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        shader: &str,
        format: wgpu::TextureFormat,
    ) -> Live {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("live stamp"),
            source: wgpu::ShaderSource::Wgsl(include_str!("live.wgsl").into()),
        });
        let stamp = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("live stamp"),
            layout: None,
            module: &module,
            entry_point: Some("stamp"),
            compilation_options: Default::default(),
            cache: None,
        });
        let view = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("live view"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shading = ShaderPass::with_format(&device, shader, format);
        Live {
            device,
            queue,
            stamp,
            shading,
            view,
            copy: None,
            tables: None,
        }
    }

    /// The map the copy shows, if any.
    pub fn map(&self) -> Option<MapId> {
        self.copy.as_ref().map(|c| c.area.map)
    }

    pub fn clear(&mut self) {
        self.copy = None;
    }

    /// True when the copy covers the same pixels as `base`.
    pub fn same_area(&self, base: &LiveBase) -> bool {
        self.copy.as_ref().is_some_and(|c| c.area == base.area)
    }

    /// Replaces the copy with `base`.
    pub fn set_base(&mut self, base: LiveBase) {
        let a = base.area;
        if self.copy.as_ref().map(|c| c.area) != Some(a) {
            self.tables = None;
        }
        let size = wgpu::Extent3d {
            width: a.w,
            height: a.h,
            depth_or_array_layers: 1,
        };
        let reuse = self.copy.take().filter(|c| c.texture.size() == size);
        let texture = match reuse {
            Some(c) => c.texture,
            None => self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("live heights"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }),
        };
        self.queue.write_texture(
            texture.as_image_copy(),
            bytemuck::cast_slice(&base.values),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(a.w * 4),
                rows_per_image: None,
            },
            size,
        );
        let view = texture.create_view(&Default::default());
        let shade = self.shading.bind_group(&self.device, &self.view, &view);
        self.copy = Some(Copy {
            area: a,
            texture,
            shade,
        });
    }

    /// Whether the copy can show stroke `s`: add (with or without a target) and roughen.
    /// Smooth and flatten read neighbours or the stroke start; they stay on the CPU path.
    pub fn supports(s: &BrushStroke) -> bool {
        matches!(s.mode, BrushMode::Add | BrushMode::Roughen)
    }

    /// Applies `s`'s stamps to the copy and submits the work. The part outside the copy is
    /// left to the CPU render. False when `s` is not `supports`ed or there is no copy.
    pub fn stamp(&mut self, seed: u64, s: &BrushStroke) -> bool {
        let Some(copy) = &self.copy else {
            return false;
        };
        if !Live::supports(s) {
            return false;
        }
        let a = copy.area;
        let frame = a.frame();
        let texel = (1i64 << a.level) as f64;
        // Global pixel coordinate to texel coordinate, and the box of each stamp.
        let to_texel = |g: f64, o: i64, t0: u32| (g - o as f64) / texel - t0 as f64;
        let (mut lo, mut hi) = ((i64::MAX, i64::MAX), (i64::MIN, i64::MIN));
        let mut words: Vec<u8> = Vec::with_capacity(s.points.len() * STAMP_BYTES);
        let raise = s.strength > 0.0;
        let core = mmc_core::brush_core(s.falloff) as f32;
        for p in &s.points {
            let (cx, cy, r) = (p.at.x / a.mpp, p.at.y / a.mpp, p.radius / a.mpp);
            let x0 = to_texel(cx - r, a.origin.0, a.x0).floor() as i64;
            let x1 = to_texel(cx + r, a.origin.0, a.x0).ceil() as i64;
            let y0 = to_texel(cy - r, a.origin.1, a.y0).floor() as i64;
            let y1 = to_texel(cy + r, a.origin.1, a.y0).ceil() as i64;
            let (x0, x1) = (x0.max(0), x1.min(a.w as i64));
            let (y0, y1) = (y0.max(0), y1.min(a.h as i64));
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            (lo, hi) = ((lo.0.min(x0), lo.1.min(y0)), (hi.0.max(x1), hi.1.max(y1)));
            let flags = s.target.is_some() as u32
                | (raise as u32) << 1
                | ((s.mode == BrushMode::Roughen) as u32) << 2;
            let f = [
                (cx - frame.0 as f64) as f32,
                (cy - frame.1 as f64) as f32,
                r as f32,
                s.strength * p.pressure,
                p.pressure,
                core,
                s.target.unwrap_or(0.0),
            ];
            f.iter()
                .for_each(|v| words.extend_from_slice(&v.to_le_bytes()));
            words.extend_from_slice(&flags.to_le_bytes());
        }
        if words.is_empty() {
            return true;
        }
        let noisy = s.detail.amount > 0.0 || s.mode == BrushMode::Roughen;
        let detail = if noisy {
            s.detail.amount.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if noisy {
            self.tables(seed, s.detail.scale);
        }
        let (octaves, total) = match (&self.tables, noisy) {
            (Some(t), true) => (t.octaves, t.total),
            _ => (0, 1.0),
        };
        let sample = a.sample(a.x0, a.y0);
        let mut params: Vec<u8> = Vec::with_capacity(48);
        for v in [lo.0 as i32, lo.1 as i32] {
            params.extend_from_slice(&v.to_le_bytes());
        }
        let size = ((hi.0 - lo.0) as u32, (hi.1 - lo.1) as u32);
        let ints = [
            size.0,
            size.1,
            (sample.0 - frame.0) as u32,
            (sample.1 - frame.1) as u32,
            a.level,
            (words.len() / STAMP_BYTES) as u32,
        ];
        ints.iter()
            .for_each(|v| params.extend_from_slice(&v.to_le_bytes()));
        for v in [detail, total] {
            params.extend_from_slice(&v.to_le_bytes());
        }
        for v in [octaves, 0] {
            params.extend_from_slice(&v.to_le_bytes());
        }
        let d = &self.device;
        let init = |label, contents: &[u8], usage| {
            d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage,
            })
        };
        let params = init("live params", &params, wgpu::BufferUsages::UNIFORM);
        let stamps = init("live stamps", &words, wgpu::BufferUsages::STORAGE);
        let empty;
        let tables = match &self.tables {
            Some(t) if noisy => &t.buffer,
            _ => {
                empty = init("live tables", &[0; 16], wgpu::BufferUsages::STORAGE);
                &empty
            }
        };
        let Some(copy) = &self.copy else {
            return false;
        };
        let view = copy.texture.create_view(&Default::default());
        let bind = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("live stamp"),
            layout: &self.stamp.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: stamps.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: tables.as_entire_binding(),
                },
            ],
        });
        let mut encoder = d.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.stamp);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(size.0.div_ceil(8), size.1.div_ceil(8), 1);
        }
        self.queue.submit([encoder.finish()]);
        true
    }

    /// Builds the detail tables of `seed` and `scale` over the copy unless they are current.
    fn tables(&mut self, seed: u64, scale: f64) {
        let Some(copy) = &self.copy else { return };
        let key = (seed, scale.to_bits());
        if self.tables.as_ref().is_some_and(|t| t.key == key) {
            return;
        }
        let a = copy.area;
        let (s0, s1) = (
            a.sample(a.x0, a.y0),
            a.sample(a.x0 + a.w - 1, a.y0 + a.h - 1),
        );
        let t = mmc_core::detail_tables(seed, scale, a.mpp, (s0.0, s1.0), (s0.1, s1.1), a.level);
        let n = t.octaves.len();
        let mut words: Vec<u32> = vec![0; n * 8];
        for (o, oct) in t.octaves.iter().enumerate() {
            let at = |w: &Vec<u32>| w.len() as u32;
            words[o * 8] = (oct.amp as f32).to_bits();
            for (slot, nodes) in [(1, &oct.node_x), (2, &oct.node_y)] {
                words[o * 8 + slot] = at(&words);
                for &(i, f) in nodes {
                    words.extend([i, (f as f32).to_bits()]);
                }
            }
            for (slot, vals) in [(3, &oct.cols), (4, &oct.rows)] {
                words[o * 8 + slot] = at(&words);
                for &v in vals {
                    words.extend([v as u32, (v >> 32) as u32]);
                }
            }
        }
        words.resize(words.len().max(4), 0);
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("live tables"),
                contents: &bytes,
                usage: wgpu::BufferUsages::STORAGE,
            });
        self.tables = Some(Tables {
            key,
            buffer,
            octaves: n as u32,
            total: t.total as f32,
        });
    }

    /// Sets up drawing the copy for `view` into a target whose pixel (0, 0) is image pixel
    /// `offset`; `None` when the copy shows another map or there is none.
    pub fn frame(&self, view: &View, offset: [f32; 2]) -> Option<LiveFrame> {
        let copy = self.copy.as_ref().filter(|c| c.area.map == view.map)?;
        let a = copy.area;
        let m = a.mpp * (1u64 << a.level) as f64;
        let mut u = Vec::with_capacity(64);
        let floats = [
            offset[0],
            offset[1],
            (view.min.x / m) as f32,
            (view.min.y / m) as f32,
            ((view.max.x - view.min.x) / m / view.width as f64) as f32,
            ((view.max.y - view.min.y) / m / view.height as f64) as f32,
        ];
        floats
            .iter()
            .for_each(|f| u.extend_from_slice(&f.to_le_bytes()));
        let ints = [a.x0, a.y0, a.x0, a.y0, a.x0 + a.w - 1, a.y0 + a.h - 1];
        ints.iter()
            .for_each(|i| u.extend_from_slice(&(*i as i32).to_le_bytes()));
        u.extend_from_slice(&(m as f32).to_le_bytes());
        u.resize(64, 0);
        self.queue.write_buffer(&self.view, 0, &u);
        Some(LiveFrame {
            pipeline: self.shading.pipeline().clone(),
            bind: copy.shade.clone(),
        })
    }

    /// Draws the copy for `view` to an image; needs a `TARGET_FORMAT` copy. For tests.
    pub fn render(&self, view: View) -> Result<Image> {
        let target = self.target(&view);
        self.submit_frame(&view, &target);
        let rgba = read_texture(&self.device, &self.queue, &target, view.width, view.height)?;
        Ok(Image {
            width: view.width,
            height: view.height,
            rgba,
        })
    }

    /// Draws the copy for `view` offscreen and submits it, as a frame does. For the bench.
    pub fn draw(&self, view: View) {
        self.submit_frame(&view, &self.target(&view));
    }

    /// Waits until the GPU has done the work submitted so far.
    pub fn wait(&self) -> Result<()> {
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map(|_| ())
            .map_err(|e| crate::Error::Gpu(e.to_string()))
    }

    fn target(&self, view: &View) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("live render"),
            size: wgpu::Extent3d {
                width: view.width,
                height: view.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Records and submits one draw of the copy for `view` into `target`, as an app frame does.
    fn submit_frame(&self, view: &View, target: &wgpu::Texture) {
        let frame = self.frame(view, [0.0, 0.0]);
        let target_view = target.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("live render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if let Some(f) = &frame {
                f.draw(&mut pass);
            }
        }
        self.queue.submit([encoder.finish()]);
    }

    /// The copy's values, level pixels row by row; waits for the GPU. For tests.
    pub fn heights(&self) -> Result<Vec<f32>> {
        let Some(c) = &self.copy else {
            return Ok(vec![]);
        };
        let bytes = read_texture(&self.device, &self.queue, &c.texture, c.area.w, c.area.h)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect())
    }
}
