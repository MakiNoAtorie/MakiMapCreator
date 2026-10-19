//! Map canvas: the renderer draws the visible view on a worker; the UI shows the last result.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, ColorImage, Rect, TextureHandle, TextureOptions};
use mmc_core::{Kinds, Layer, MapId, Project};
use mmc_render::{Image, Renderer, ShaderPass, View};

use crate::view::MapView;

/// Largest render target side; the renderer splits bigger views into tiles of this size.
const RENDER_TILE: u32 = 2048;

pub type SharedProject = Arc<Mutex<Option<Project>>>;

/// What the worker draws: a view and the layers shown, bottom first.
#[derive(Clone, PartialEq)]
struct Req {
    view: View,
    layers: Vec<Layer>,
}

/// A renderer with the draw passes of `layers`, taken from the kinds that declare them.
fn renderer_for(rs: &eframe::egui_wgpu::RenderState, kinds: &Kinds, layers: &[Layer]) -> Renderer {
    let mut r = Renderer::from_device(rs.device.clone(), rs.queue.clone());
    for &l in layers {
        let pass = kinds
            .all()
            .flat_map(|k| k.render_passes())
            .find(|p| p.layer == l);
        if let Some(p) = pass {
            r.register(l, Box::new(ShaderPass::new(&rs.device, p.shader)));
        }
    }
    r
}

pub struct Canvas {
    requests: Sender<(Req, u64)>,
    results: Receiver<(Req, u64, mmc_render::Result<Image>)>,
    requested: Option<Req>,
    shown: Option<(Req, TextureHandle)>,
    /// Bumped by `invalidate`; a render carries the value it was requested under.
    epoch: u64,
    fresh_epoch: u64,
    pub error: Option<String>,
}

impl Canvas {
    pub fn new(
        render_state: &eframe::egui_wgpu::RenderState,
        project: SharedProject,
        kinds: std::sync::Arc<Kinds>,
        ctx: egui::Context,
    ) -> Canvas {
        let rs = render_state.clone();
        let mut renderers: Vec<(Vec<Layer>, Renderer)> = vec![];
        let (requests, jobs) = channel::<(Req, u64)>();
        let (done, results) = channel();
        std::thread::spawn(move || {
            while let Ok((mut req, mut epoch)) = jobs.recv() {
                // A newer request replaces older ones (ADR-0014).
                while let Ok((v, e)) = jobs.try_recv() {
                    (req, epoch) = (v, e);
                }
                // ponytail: scene reads full-resolution tiles under the project lock; pyramid levels (#9) cut this when zoomed out.
                let at = match renderers.iter().position(|r| r.0 == req.layers) {
                    Some(i) => i,
                    None => {
                        renderers
                            .push((req.layers.clone(), renderer_for(&rs, &kinds, &req.layers)));
                        renderers.len() - 1
                    }
                };
                let renderer = &renderers[at].1;
                let scene = match project.lock().unwrap().as_ref() {
                    Some(p) => renderer.scene(p, req.view),
                    None => continue,
                };
                let result = scene.and_then(|s| renderer.render(&s, RENDER_TILE));
                if done.send((req, epoch, result)).is_err() {
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
            epoch: 0,
            fresh_epoch: 0,
            error: None,
        }
    }

    /// Forgets the shown image, for example after another project opens.
    pub fn clear(&mut self) {
        self.requested = None;
        self.shown = None;
        self.error = None;
    }

    /// Forces a new render of the same view, for example after an edit; returns the epoch to wait for.
    pub fn invalidate(&mut self) -> u64 {
        self.epoch += 1;
        self.requested = None;
        self.epoch
    }

    /// True once a render requested at or after `epoch` has arrived.
    pub fn fresh(&self, epoch: u64) -> bool {
        self.fresh_epoch >= epoch
    }

    /// Requests `view` when it changed, takes finished renders and paints the latest into `rect`.
    pub fn show(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        map: MapId,
        layers: Vec<Layer>,
        map_view: &MapView,
    ) {
        let ppp = ui.ctx().pixels_per_point();
        let (min, max) = map_view.visible(rect);
        let view = View {
            map,
            min,
            max,
            width: (rect.width() * ppp).round() as u32,
            height: (rect.height() * ppp).round() as u32,
        };
        if layers.is_empty() {
            self.requested = None;
            self.shown = None;
            return;
        }
        let req = Req { view, layers };
        if self.requested.as_ref() != Some(&req) && view.width > 0 && view.height > 0 {
            self.requested = Some(req.clone());
            let _ = self.requests.send((req.clone(), self.epoch));
        }
        while let Ok((v, epoch, result)) = self.results.try_recv() {
            self.fresh_epoch = self.fresh_epoch.max(epoch);
            // A render for another map or layer set is out of date.
            if v.view.map != req.view.map || v.layers != req.layers {
                continue;
            }
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
                Err(e) => self.error = Some(crate::errors::render_text(&e)),
            }
        }
        // The last render stands in, moved and scaled, until the new one arrives.
        if let Some((v, tex)) = &self.shown {
            let r = Rect::from_min_max(
                map_view.screen_at(rect, v.view.min),
                map_view.screen_at(rect, v.view.max),
            );
            let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter_at(rect).image(tex.id(), r, uv, Color32::WHITE);
        }
    }
}
