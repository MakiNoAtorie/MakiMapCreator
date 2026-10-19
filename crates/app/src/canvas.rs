//! Map canvas: the renderer draws the visible view on a worker; the UI shows the last result.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, ColorImage, Rect, TextureHandle, TextureOptions};
use mmc_core::{Layer, MapId, Project};
use mmc_render::{HeightPass, Image, Renderer, View};

use crate::view::MapView;

/// Largest render target side; the renderer splits bigger views into tiles of this size.
const RENDER_TILE: u32 = 2048;

pub type SharedProject = Arc<Mutex<Option<Project>>>;

pub struct Canvas {
    requests: Sender<View>,
    results: Receiver<(View, mmc_render::Result<Image>)>,
    requested: Option<View>,
    shown: Option<(View, TextureHandle)>,
    pub error: Option<String>,
}

impl Canvas {
    pub fn new(
        render_state: &eframe::egui_wgpu::RenderState,
        project: SharedProject,
        ctx: egui::Context,
    ) -> Canvas {
        let device = render_state.device.clone();
        let mut renderer = Renderer::from_device(device.clone(), render_state.queue.clone());
        renderer.register(Layer::Height, Box::new(HeightPass::new(&device)));
        let (requests, jobs) = channel::<View>();
        let (done, results) = channel();
        std::thread::spawn(move || {
            while let Ok(mut view) = jobs.recv() {
                // A newer request replaces older ones (ADR-0014).
                while let Ok(v) = jobs.try_recv() {
                    view = v;
                }
                // ponytail: scene reads full-resolution tiles under the project lock; pyramid levels (#9) cut this when zoomed out.
                let scene = match project.lock().unwrap().as_ref() {
                    Some(p) => renderer.scene(p, view),
                    None => continue,
                };
                let result = scene.and_then(|s| renderer.render(&s, RENDER_TILE));
                if done.send((view, result)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Canvas {
            requests,
            results,
            requested: None,
            shown: None,
            error: None,
        }
    }

    /// Forgets the shown image, for example after another project opens.
    pub fn clear(&mut self) {
        self.requested = None;
        self.shown = None;
        self.error = None;
    }

    /// Requests `view` when it changed, takes finished renders and paints the latest into `rect`.
    pub fn show(&mut self, ui: &egui::Ui, rect: Rect, map: MapId, map_view: &MapView) {
        let ppp = ui.ctx().pixels_per_point();
        let (min, max) = map_view.visible(rect);
        let view = View {
            map,
            min,
            max,
            width: (rect.width() * ppp).round() as u32,
            height: (rect.height() * ppp).round() as u32,
        };
        if self.requested != Some(view) && view.width > 0 && view.height > 0 {
            self.requested = Some(view);
            let _ = self.requests.send(view);
        }
        while let Ok((v, result)) = self.results.try_recv() {
            match result {
                Ok(img) => {
                    let pixels = ColorImage::from_rgba_unmultiplied(
                        [img.width as usize, img.height as usize],
                        &img.rgba,
                    );
                    let tex = match self.shown.take() {
                        Some((_, mut t)) => {
                            t.set(pixels, TextureOptions::LINEAR);
                            t
                        }
                        None => ui.ctx().load_texture("map", pixels, TextureOptions::LINEAR),
                    };
                    self.shown = Some((v, tex));
                    self.error = None;
                }
                Err(mmc_render::Error::OutsideMap) => self.shown = None,
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        // The last render stands in, moved and scaled, until the new one arrives.
        if let Some((v, tex)) = &self.shown {
            let r = Rect::from_min_max(
                map_view.screen_at(rect, v.min),
                map_view.screen_at(rect, v.max),
            );
            let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter_at(rect).image(tex.id(), r, uv, Color32::WHITE);
        }
    }
}
