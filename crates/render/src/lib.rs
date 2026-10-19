//! Offscreen wgpu renderer (seam 2): draws a map area to an image, tile by tile, with no window.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use mmc_core::{Layer, MapId, Project, RootPoint};

mod height;
mod levels;
pub use height::HeightPass;
mod kind;
pub use kind::LandPass;
mod summary;

/// Format of every render target. Not sRGB, so image bytes are the shader output.
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[derive(Debug)]
pub enum Error {
    Core(mmc_core::Error),
    Gpu(String),
    Png(png::EncodingError),
    Io(std::io::Error),
    OutsideMap,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Core(e) => e.fmt(f),
            Error::Gpu(e) => write!(f, "the graphics device failed: {e}"),
            Error::Png(e) => write!(f, "the PNG export failed: {e}"),
            Error::Io(e) => write!(f, "the export file failed: {e}"),
            Error::OutsideMap => write!(f, "the area is outside the map"),
        }
    }
}

impl std::error::Error for Error {}

impl From<mmc_core::Error> for Error {
    fn from(e: mmc_core::Error) -> Self {
        Error::Core(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A map area in root space drawn to `width` x `height` image pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub map: MapId,
    pub min: RootPoint,
    pub max: RootPoint,
    pub width: u32,
    pub height: u32,
}

/// RGBA8 pixels, row by row, no padding.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Layer values of a map area in map pixels; `x0..x0+w`, `y0..y0+h`.
struct Raster {
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    values: Vec<f32>,
}

/// The layer data a view needs, read from the project. Owns its data so a worker can render it.
pub struct Scene {
    view: View,
    metres_per_pixel: f64,
    rasters: Vec<(Layer, Raster)>,
    complete: bool,
    summaries: Vec<mmc_core::ChildSummary>,
}

impl Scene {
    /// False while part of the scene is a stand-in from a coarser level.
    pub fn is_complete(&self) -> bool {
        self.complete
    }
}

/// What a draw pass gets for one target tile.
pub struct PassInput<'a> {
    /// R32Float values of the pass's layer for this tile, origin at `ViewUniform` `tex_origin`.
    pub layer: &'a wgpu::TextureView,
    /// The view uniform (see `VIEW_WGSL`).
    pub view: &'a wgpu::Buffer,
}

/// Draws one layer into the current target. Map kinds register these per layer.
pub trait DrawPass: Send + Sync {
    fn draw(&self, device: &wgpu::Device, input: &PassInput<'_>, pass: &mut wgpu::RenderPass<'_>);
}

/// WGSL declaration of the view uniform that every pass binds.
pub const VIEW_WGSL: &str = "
struct ViewUniform {
    // Image pixel of this target's top-left corner.
    tile_offset: vec2f,
    // Map pixel at image pixel (0, 0), and map pixels per image pixel.
    origin_px: vec2f,
    scale: vec2f,
    // Map pixel of layer texel (0, 0).
    tex_origin: vec2i,
    // Map pixels that have data, inclusive. Outside is outside the map.
    lo: vec2i,
    hi: vec2i,
    metres_per_pixel: f32,
}
";

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    passes: Vec<(Layer, Box<dyn DrawPass>)>,
}

impl Renderer {
    /// A renderer on the WARP software adapter (DirectX 12); needs no GPU.
    pub fn warp() -> Result<Renderer> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            force_fallback_adapter: true,
            ..Default::default()
        }))
        .map_err(|e| Error::Gpu(e.to_string()))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .map_err(|e| Error::Gpu(e.to_string()))?;
        Ok(Renderer::from_device(device, queue))
    }

    /// A renderer on a device the caller owns, for example the app window's device.
    pub fn from_device(device: wgpu::Device, queue: wgpu::Queue) -> Renderer {
        Renderer {
            device,
            queue,
            passes: Vec::new(),
        }
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Adds a pass for `layer`. Passes draw in the order they were registered.
    pub fn register(&mut self, layer: Layer, pass: Box<dyn DrawPass>) {
        self.passes.push((layer, pass));
    }

    /// Reads full-resolution tiles of the layers that registered passes draw, for the area
    /// of `view`. Blocks on disk; export uses it. The screen uses `scene_cached`.
    pub fn scene(&self, project: &Project, view: View) -> Result<Scene> {
        let map = levels::find_map(project, view)?;
        self.assemble(map, view, 0, |layer, tx, ty| {
            Ok(Some((0, project.layer_tile(map.id, layer, tx, ty)?.into())))
        })
    }

    /// Renders `scene` in targets of at most `tile` x `tile` pixels; any tile size gives the same image.
    pub fn render(&self, scene: &Scene, tile: u32) -> Result<Image> {
        let v = scene.view;
        let tile = tile
            .min(self.device.limits().max_texture_dimension_2d)
            .max(1);
        let mut img = Image {
            width: v.width,
            height: v.height,
            rgba: vec![0; (v.width * v.height * 4) as usize],
        };
        for gy in (0..v.height).step_by(tile as usize) {
            for gx in (0..v.width).step_by(tile as usize) {
                let (w, h) = (tile.min(v.width - gx), tile.min(v.height - gy));
                let pixels = self.render_tile(scene, gx, gy, w, h)?;
                for row in 0..h {
                    let dst = (((gy + row) * v.width + gx) * 4) as usize;
                    let src = (row * w * 4) as usize;
                    img.rgba[dst..dst + (w * 4) as usize]
                        .copy_from_slice(&pixels[src..src + (w * 4) as usize]);
                }
            }
        }
        summary::tint(scene, &mut img);
        Ok(img)
    }

    fn render_tile(&self, scene: &Scene, gx: u32, gy: u32, w: u32, h: u32) -> Result<Vec<u8>> {
        let v = scene.view;
        let m = scene.metres_per_pixel;
        let origin = (v.min.x / m, v.min.y / m);
        let scale = (
            (v.max.x - v.min.x) / m / v.width as f64,
            (v.max.y - v.min.y) / m / v.height as f64,
        );
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("render tile"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        // Upload each layer's texels that this tile can read, then draw.
        let mut inputs = Vec::new();
        for (layer, r) in &scene.rasters {
            let texels = |g0: u32, g1: u32, o: f64, s: f64, r0: u32, rn: u32| {
                let lo = ((o + g0 as f64 * s).floor() - 2.0).max(r0 as f64) as u32;
                let hi = ((o + g1 as f64 * s).floor() + 3.0).min((r0 + rn) as f64) as u32;
                (lo, hi)
            };
            let (tx0, tx1) = texels(gx, gx + w, origin.0, scale.0, r.x0, r.w);
            let (ty0, ty1) = texels(gy, gy + h, origin.1, scale.1, r.y0, r.h);
            if tx0 >= tx1 || ty0 >= ty1 {
                continue;
            }
            let (tw, th) = (tx1 - tx0, ty1 - ty0);
            let mut bytes = Vec::with_capacity((tw * th * 4) as usize);
            for y in ty0..ty1 {
                let start = ((y - r.y0) * r.w + tx0 - r.x0) as usize;
                for v in &r.values[start..start + tw as usize] {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
            }
            let size = wgpu::Extent3d {
                width: tw,
                height: th,
                depth_or_array_layers: 1,
            };
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("layer"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                texture.as_image_copy(),
                &bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(tw * 4),
                    rows_per_image: None,
                },
                size,
            );
            let mut u = Vec::with_capacity(64);
            for f in [gx as f32, gy as f32, origin.0 as f32, origin.1 as f32] {
                u.extend_from_slice(&f.to_le_bytes());
            }
            for f in [scale.0 as f32, scale.1 as f32] {
                u.extend_from_slice(&f.to_le_bytes());
            }
            let ints = [tx0, ty0, r.x0, r.y0, r.x0 + r.w - 1, r.y0 + r.h - 1];
            for i in ints {
                u.extend_from_slice(&(i as i32).to_le_bytes());
            }
            u.extend_from_slice(&(m as f32).to_le_bytes());
            u.resize(64, 0);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("view"),
                size: 64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&buffer, 0, &u);
            inputs.push((*layer, texture.create_view(&Default::default()), buffer));
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render tile"),
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
            for (layer, draw) in &self.passes {
                if let Some((_, layer_view, view)) = inputs.iter().find(|i| i.0 == *layer) {
                    let input = PassInput {
                        layer: layer_view,
                        view,
                    };
                    draw.draw(&self.device, &input, &mut pass);
                }
            }
        }
        // Rows of a texture-to-buffer copy are padded to 256 bytes.
        let padded = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| Error::Gpu(e.to_string()))?;
        rx.recv()
            .map_err(|e| Error::Gpu(e.to_string()))?
            .map_err(|e| Error::Gpu(e.to_string()))?;
        let data = slice
            .get_mapped_range()
            .map_err(|e| Error::Gpu(e.to_string()))?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in data.chunks(padded as usize) {
            out.extend_from_slice(&row[..(w * 4) as usize]);
        }
        Ok(out)
    }
}

/// Renders `scene` on a worker thread and writes it to `path` as an RGBA PNG.
pub fn export_png(
    renderer: Arc<Renderer>,
    scene: Scene,
    tile: u32,
    path: PathBuf,
) -> JoinHandle<Result<()>> {
    std::thread::spawn(move || {
        let img = renderer.render(&scene, tile)?;
        let file = std::fs::File::create(&path).map_err(Error::Io)?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), img.width, img.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(Error::Png)?;
        writer.write_image_data(&img.rgba).map_err(Error::Png)?;
        writer.finish().map_err(Error::Png)
    })
}
