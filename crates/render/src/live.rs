//! Live brush (ADR-0025): a display-only GPU copy of the drawn heights that stamps change at once.

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
    /// First global level-0 pixel of level pixel (`x`, `y`).
    fn block(&self, x: u32, y: u32) -> (i64, i64) {
        (
            self.origin.0 + ((x as i64) << self.level),
            self.origin.1 + ((y as i64) << self.level),
        )
    }

    /// Global level-0 pixels the copy covers, inclusive, per axis.
    fn pixels(&self) -> ((i64, i64), (i64, i64)) {
        let (a, b) = (
            self.block(self.x0, self.y0),
            self.block(self.x0 + self.w, self.y0 + self.h),
        );
        ((a.0, b.0 - 1), (a.1, b.1 - 1))
    }

    /// First pixel of the frame the shader works in: the first detail node, a multiple of 4.
    fn frame(&self) -> (i64, i64) {
        let (x, y) = self.block(self.x0, self.y0);
        (x.div_euclid(4) * 4, y.div_euclid(4) * 4)
    }
}

/// Level-0 pixels per axis a texel averages a stamp over; all of them up to level 2.
// ponytail: 4 of 2^level from level 3 on; more if zoomed-out hand-overs show texture.
const SUB_MAX: u32 = 4;

/// `live.wgsl` `Params`, one per dispatch.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    box0: [i32; 2],
    size: [u32; 2],
    g0: [i32; 2],
    /// `RIDGE_HEIGHTS`.
    ridge: [f32; 2],
    level: u32,
    count: u32,
    sub: u32,
    detail: f32,
    octaves: u32,
    first: u32,
    reach: f32,
    pad: u32,
}

/// `live.wgsl` `Stamp`; `centre` is in frame pixels.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Stamp {
    centre: [f32; 2],
    radius: f32,
    amount: f32,
    pressure: f32,
    core: f32,
    goal: f32,
    mode: u32,
}

/// `live.wgsl` stamp modes.
const ADD: u32 = 0;
const RAISE_TO: u32 = 1;
const LOWER_TO: u32 = 2;
const ROUGHEN: u32 = 3;
const FLATTEN: u32 = 4;
const SMOOTH: u32 = 5;

/// Where a flatten part's level comes from; other modes ignore it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlattenLevel {
    /// The CPU stroke's level.
    At(f32),
    /// The copy under the part's first stamp: the part starts the stroke.
    Sample,
    /// The level an earlier part of the stroke left on the copy.
    Kept,
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
    /// The texture's values before the running dispatch, which reads neighbours.
    before: wgpu::Texture,
    /// Smooth's row means.
    rows: wgpu::Texture,
    shade: wgpu::BindGroup,
}

/// Detail tables for (seed, scale bits) over the copy's area, as `live.wgsl` reads them.
struct Tables {
    key: (u64, u64),
    buffer: wgpu::Buffer,
    octaves: u32,
}

/// The live copy: stamps run in compute passes (`live.wgsl`), shading is the kind's height pass.
pub struct Live {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    stamp: wgpu::ComputePipeline,
    sample: wgpu::ComputePipeline,
    smooth_rows: wgpu::ComputePipeline,
    shading: ShaderPass,
    view: wgpu::Buffer,
    copy: Option<Copy>,
    tables: Option<Tables>,
    /// Bound as the tables when a stroke has no noise.
    no_tables: wgpu::Buffer,
    /// The flatten level.
    level: wgpu::Buffer,
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

/// A texel box of the copy: `lo` inclusive, `hi` exclusive.
#[derive(Clone, Copy)]
struct Texels {
    lo: (i64, i64),
    hi: (i64, i64),
}

impl Texels {
    fn union(self, o: Texels) -> Texels {
        Texels {
            lo: (self.lo.0.min(o.lo.0), self.lo.1.min(o.lo.1)),
            hi: (self.hi.0.max(o.hi.0), self.hi.1.max(o.hi.1)),
        }
    }

    /// Grown by `d` on every side, inside `w` x `h`.
    fn grow(self, d: i64, w: u32, h: u32) -> Texels {
        Texels {
            lo: ((self.lo.0 - d).max(0), (self.lo.1 - d).max(0)),
            hi: ((self.hi.0 + d).min(w as i64), (self.hi.1 + d).min(h as i64)),
        }
    }

    fn size(self) -> [u32; 2] {
        [
            (self.hi.0 - self.lo.0) as u32,
            (self.hi.1 - self.lo.1) as u32,
        ]
    }
}

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
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let buffer = |ty| wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let storage = |read_only| buffer(wgpu::BufferBindingType::Storage { read_only });
        let image = wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::ReadWrite,
            format: wgpu::TextureFormat::R32Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("live stamp"),
            entries: &[
                entry(0, buffer(wgpu::BufferBindingType::Uniform)),
                entry(1, image),
                entry(2, storage(true)),
                entry(3, storage(true)),
                entry(
                    4,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(5, storage(false)),
                entry(6, image),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("live stamp"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |name| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(name),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let (stamp, sample, smooth_rows) = (
            pipeline("stamp"),
            pipeline("sample"),
            pipeline("smooth_rows"),
        );
        let view = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("live view"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shading = ShaderPass::with_format(&device, shader, format);
        let small = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 16,
                usage,
                mapped_at_creation: false,
            })
        };
        let no_tables = small("live no tables", wgpu::BufferUsages::STORAGE);
        let level = small(
            "live level",
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        Live {
            device,
            queue,
            layout,
            stamp,
            sample,
            smooth_rows,
            shading,
            view,
            copy: None,
            tables: None,
            no_tables,
            level,
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
        let make = |label, usage| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Float,
                usage,
                view_formats: &[],
            })
        };
        use wgpu::TextureUsages as U;
        let (texture, before, rows) = match reuse {
            Some(c) => (c.texture, c.before, c.rows),
            None => (
                make(
                    "live heights",
                    U::TEXTURE_BINDING | U::STORAGE_BINDING | U::COPY_DST | U::COPY_SRC,
                ),
                make("live before", U::TEXTURE_BINDING | U::COPY_DST),
                make("live rows", U::STORAGE_BINDING),
            ),
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
            before,
            rows,
            shade,
        });
    }

    /// Stamps `s` inside the copy; false with no copy, or a `Sample` level off the copy.
    pub fn stamp(&mut self, seed: u64, s: &BrushStroke, level: FlattenLevel) -> bool {
        let Some(copy) = &self.copy else {
            return false;
        };
        let a = copy.area;
        let frame = a.frame();
        let texel = (1i64 << a.level) as f64;
        let to_texel = |g: f64, o: i64, t0: u32| (g - o as f64) / texel - t0 as f64;
        let core = mmc_core::brush_core(s.falloff) as f32;
        let mode = match (s.mode, s.target) {
            (BrushMode::Smooth, _) => SMOOTH,
            (BrushMode::Flatten, _) => FLATTEN,
            (BrushMode::Roughen, _) => ROUGHEN,
            (BrushMode::Add, None) => ADD,
            (BrushMode::Add, Some(_)) if s.strength > 0.0 => RAISE_TO,
            (BrushMode::Add, Some(_)) => LOWER_TO,
        };
        let mut sample = None;
        if mode == FLATTEN && level == FlattenLevel::Sample {
            let p = s.points.first().map(|p| (p.at.x / a.mpp, p.at.y / a.mpp));
            let t = p.map(|(x, y)| {
                let t = |g: f64, o: i64, t0: u32| {
                    (g.floor() as i64 - o).div_euclid(1 << a.level) - t0 as i64
                };
                (t(x, a.origin.0, a.x0), t(y, a.origin.1, a.y0))
            });
            match t {
                Some((x, y)) if (0..a.w as i64).contains(&x) && (0..a.h as i64).contains(&y) => {
                    sample = Some([x as i32, y as i32])
                }
                _ => return false,
            }
        }
        let mut boxes: Vec<(Texels, f32)> = Vec::with_capacity(s.points.len());
        let mut stamps: Vec<Stamp> = Vec::with_capacity(s.points.len());
        for p in &s.points {
            let (cx, cy, r) = (p.at.x / a.mpp, p.at.y / a.mpp, p.radius / a.mpp);
            let b = Texels {
                lo: (
                    (to_texel(cx - r, a.origin.0, a.x0).floor() as i64).max(0),
                    (to_texel(cy - r, a.origin.1, a.y0).floor() as i64).max(0),
                ),
                hi: (
                    (to_texel(cx + r, a.origin.0, a.x0).ceil() as i64).min(a.w as i64),
                    (to_texel(cy + r, a.origin.1, a.y0).ceil() as i64).min(a.h as i64),
                ),
            };
            if b.lo.0 >= b.hi.0 || b.lo.1 >= b.hi.1 {
                continue;
            }
            // Smooth's box: `BrushMode::Smooth`'s 2k + 1 pixels as a texel half width.
            let k = (r / 4.0).ceil().max(1.0);
            boxes.push((b, ((k + 0.5) / texel - 0.5) as f32));
            stamps.push(Stamp {
                centre: [(cx - frame.0 as f64) as f32, (cy - frame.1 as f64) as f32],
                radius: r as f32,
                amount: s.strength * p.pressure,
                pressure: p.pressure,
                core,
                goal: s.target.unwrap_or(0.0),
                mode,
            });
        }
        if stamps.is_empty() && sample.is_none() {
            return true;
        }
        let noisy = s.noisy();
        if noisy {
            self.tables(seed, s.detail.scale);
        }
        let octaves = match (&self.tables, noisy) {
            (Some(t), true) => t.octaves,
            _ => 0,
        };
        if let (FLATTEN, FlattenLevel::At(l)) = (mode, level) {
            self.queue
                .write_buffer(&self.level, 0, bytemuck::bytes_of(&l));
        }
        let first = a.block(a.x0, a.y0);
        let base = Params {
            g0: [(first.0 - frame.0) as i32, (first.1 - frame.1) as i32],
            level: a.level,
            sub: (1 << a.level).min(SUB_MAX),
            detail: s.detail.amount(),
            octaves,
            ridge: mmc_core::RIDGE_HEIGHTS.into(),
            ..Default::default()
        };
        let Some(copy) = &self.copy else {
            return false;
        };
        let tables = match &self.tables {
            Some(t) if noisy => &t.buffer,
            _ => &self.no_tables,
        };
        let d = &self.device;
        let stamps = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("live stamps"),
            contents: if stamps.is_empty() {
                &[0; 32]
            } else {
                bytemuck::cast_slice(&stamps)
            },
            usage: wgpu::BufferUsages::STORAGE,
        });
        let (view, before, rows) = (
            copy.texture.create_view(&Default::default()),
            copy.before.create_view(&Default::default()),
            copy.rows.create_view(&Default::default()),
        );
        let bind = |params: Params| {
            let params = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("live params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let tex = |v| wgpu::BindingResource::TextureView(v);
            let entries = [
                params.as_entire_binding(),
                tex(&view),
                stamps.as_entire_binding(),
                tables.as_entire_binding(),
                tex(&before),
                self.level.as_entire_binding(),
                tex(&rows),
            ];
            let entries: Vec<_> = entries
                .into_iter()
                .enumerate()
                .map(|(i, resource)| wgpu::BindGroupEntry {
                    binding: i as u32,
                    resource,
                })
                .collect();
            d.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("live stamp"),
                layout: &self.layout,
                entries: &entries,
            })
        };
        let mut encoder = d.create_command_encoder(&Default::default());
        let snapshot = |encoder: &mut wgpu::CommandEncoder, b: Texels| {
            let at = |t| wgpu::TexelCopyTextureInfo {
                origin: wgpu::Origin3d {
                    x: b.lo.0 as u32,
                    y: b.lo.1 as u32,
                    z: 0,
                },
                ..wgpu::Texture::as_image_copy(t)
            };
            let [width, height] = b.size();
            encoder.copy_texture_to_texture(
                at(&copy.texture),
                at(&copy.before),
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        };
        let dispatch = |pass: &mut wgpu::ComputePass, pipeline, params: Params| {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind(params), &[]);
            let [w, h] = params.size;
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        };
        if let Some(box0) = sample {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.sample);
            pass.set_bind_group(0, &bind(Params { box0, ..base }), &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        if mode == SMOOTH {
            // Each stamp reads the heights the one before it left.
            for (i, &(b, reach)) in boxes.iter().enumerate() {
                let k = reach.ceil() as i64;
                snapshot(&mut encoder, b.grow(k + 1, a.w, a.h));
                let rows = Texels {
                    lo: (b.lo.0, (b.lo.1 - k).max(0)),
                    hi: (b.hi.0, (b.hi.1 + k).min(a.h as i64)),
                };
                let mut pass = encoder.begin_compute_pass(&Default::default());
                let at = |t: Texels| Params {
                    box0: [t.lo.0 as i32, t.lo.1 as i32],
                    size: t.size(),
                    first: i as u32,
                    count: 1,
                    reach,
                    ..base
                };
                dispatch(&mut pass, &self.smooth_rows, at(rows));
                dispatch(&mut pass, &self.stamp, at(b));
            }
        } else if let Some(all) = boxes.iter().map(|b| b.0).reduce(Texels::union) {
            snapshot(&mut encoder, all.grow(1, a.w, a.h));
            let mut pass = encoder.begin_compute_pass(&Default::default());
            let params = Params {
                box0: [all.lo.0 as i32, all.lo.1 as i32],
                size: all.size(),
                count: boxes.len() as u32,
                ..base
            };
            dispatch(&mut pass, &self.stamp, params);
        }
        self.queue.submit([encoder.finish()]);
        true
    }

    /// Detail tables over the copy as u32 words: per octave amp, prefix and offsets of the node
    /// columns and rows, each a lattice corner and fraction.
    fn tables(&mut self, seed: u64, scale: f64) {
        let Some(copy) = &self.copy else { return };
        let key = (seed, scale.to_bits());
        if self.tables.as_ref().is_some_and(|t| t.key == key) {
            return;
        }
        let a = copy.area;
        let (xs, ys) = a.pixels();
        let t = mmc_core::detail_tables(seed, scale, a.mpp, xs, ys);
        let n = t.octaves.len();
        let mut words: Vec<u32> = vec![0; n * 8];
        for (o, oct) in t.octaves.iter().enumerate() {
            words[o * 8] = (oct.amp as f32).to_bits();
            words[o * 8 + 1] = oct.prefix as u32;
            words[o * 8 + 2] = (oct.prefix >> 32) as u32;
            for (slot, parts) in [(3, &oct.cols), (4, &oct.rows)] {
                words[o * 8 + slot] = words.len() as u32;
                for &((x, y), (fx, fy)) in parts {
                    words.extend([x as u32, y as u32, fx, fy]);
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
        Ok(bytemuck::pod_collect_to_vec(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Params` and `Stamp` have the offsets and sizes of their `live.wgsl` structs.
    #[test]
    fn rust_structs_match_the_shader_layout() {
        let module = naga::front::wgsl::parse_str(include_str!("live.wgsl")).unwrap();
        let mut layouter = naga::proc::Layouter::default();
        layouter.update(module.to_ctx()).unwrap();
        let wgsl = |name: &str| {
            let (h, ty) = module
                .types
                .iter()
                .find(|(_, t)| t.name.as_deref() == Some(name))
                .unwrap();
            let naga::TypeInner::Struct { members, span } = &ty.inner else {
                panic!("{name} is a struct")
            };
            assert_eq!(layouter[h].size, *span);
            let m: Vec<(String, u32)> = members
                .iter()
                .map(|m| (m.name.clone().unwrap(), m.offset))
                .collect();
            (m, *span as usize)
        };
        macro_rules! layout {
            ($t:ty, $($f:ident),*) => {
                (vec![$((stringify!($f).to_string(), std::mem::offset_of!($t, $f) as u32)),*],
                 std::mem::size_of::<$t>())
            };
        }
        assert_eq!(
            wgsl("Params"),
            layout!(
                Params, box0, size, g0, ridge, level, count, sub, detail, octaves, first, reach,
                pad
            )
        );
        assert_eq!(
            wgsl("Stamp"),
            layout!(Stamp, centre, radius, amount, pressure, core, goal, mode)
        );
        let modes = [
            ("ADD", ADD),
            ("RAISE_TO", RAISE_TO),
            ("LOWER_TO", LOWER_TO),
            ("ROUGHEN", ROUGHEN),
            ("FLATTEN", FLATTEN),
            ("SMOOTH", SMOOTH),
        ];
        for (name, v) in modes {
            let line = format!("const {name} = {v}u;");
            assert!(include_str!("live.wgsl").contains(&line), "{line}");
        }
        let consts = [
            ("RIDGE_GAIN", mmc_core::RIDGE_GAIN),
            ("VALUE_SPREAD", mmc_core::VALUE_SPREAD),
            ("RIDGED_MID", mmc_core::RIDGED_MID),
            ("RIDGED_SPREAD", mmc_core::RIDGED_SPREAD),
            ("COAST_BAND", mmc_core::COAST_BAND as f64),
            ("WATER_LEVEL", mmc_core::WATER_LEVEL as f64),
        ];
        for (name, v) in consts {
            let line = format!("const {name} = {v:?};");
            assert!(include_str!("live.wgsl").contains(&line), "{line}");
        }
    }
}
