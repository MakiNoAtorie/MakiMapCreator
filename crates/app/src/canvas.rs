//! Map canvas: the renderer draws the visible view on a worker; the UI shows the last result.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, ColorImage, Rect, TextureHandle, TextureOptions};
use mmc_core::{Kinds, Layer, MapId, Project};
use mmc_render::{Image, Renderer, ShaderPass, View};

use crate::live::{Applied, LiveBrush, LiveSource};
use crate::view::MapView;

/// Largest render target side; the renderer splits bigger views into tiles of this size.
const RENDER_TILE: u32 = 2048;
/// Wait before a stand-in render is redone with the tiles loaded meanwhile.
const STAND_IN_RETRY: Duration = Duration::from_millis(16);

pub type SharedProject = Arc<Mutex<Option<Project>>>;

/// A lock wait longer than this is logged with the holder.
const SLOW_LOCK: Duration = Duration::from_millis(50);
/// The worker that last took the project lock, for the stall log.
static HOLDER: Mutex<&str> = Mutex::new("none");

/// Locks the project on a worker, never the UI thread; gives the guard and the wait.
pub fn lock_project<'a>(
    project: &'a SharedProject,
    who: &'static str,
) -> (MutexGuard<'a, Option<Project>>, Duration) {
    let t = Instant::now();
    let before = *HOLDER.lock().unwrap();
    let guard = project.lock().unwrap_or_else(|e| e.into_inner());
    let wait = t.elapsed();
    *HOLDER.lock().unwrap() = who;
    if wait > SLOW_LOCK {
        log::warn!(
            "{who} waited {:.0} ms for the project lock (held by {before})",
            wait.as_secs_f64() * 1000.0
        );
    }
    (guard, wait)
}

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

/// A render: (request, epoch, complete, image, live base).
type Rendered = (
    Req,
    u64,
    bool,
    mmc_render::Result<Image>,
    Option<LiveSource>,
);

pub struct Canvas {
    requests: Sender<(Req, u64)>,
    results: Receiver<Rendered>,
    requested: Option<Req>,
    shown: Option<(Req, TextureHandle)>,
    /// Bumped by `invalidate`; a render carries the value it was requested under.
    epoch: u64,
    fresh_epoch: u64,
    pub error: Option<String>,
    live: LiveBrush,
}

impl Canvas {
    pub fn new(
        render_state: &eframe::egui_wgpu::RenderState,
        project: SharedProject,
        kinds: std::sync::Arc<Kinds>,
        ctx: egui::Context,
        applied: Applied,
    ) -> Canvas {
        let live = LiveBrush::new(render_state.clone());
        let rs = render_state.clone();
        let mut renderers: Vec<(Vec<Layer>, Renderer)> = vec![];
        let (requests, jobs) = channel::<(Req, u64)>();
        let (done, results) = channel();
        std::thread::spawn(move || {
            // A stand-in render is redone about once a frame until its tiles have loaded.
            let mut pending: Option<(Req, u64)> = None;
            loop {
                let next = match pending.take() {
                    Some(p) => match jobs.recv_timeout(STAND_IN_RETRY) {
                        Ok(n) => n,
                        Err(RecvTimeoutError::Timeout) => p,
                        Err(RecvTimeoutError::Disconnected) => break,
                    },
                    None => match jobs.recv() {
                        Ok(n) => n,
                        Err(_) => break,
                    },
                };
                let (mut req, mut epoch) = next;
                // A newer request replaces older ones (ADR-0014).
                while let Ok((v, e)) = jobs.try_recv() {
                    (req, epoch) = (v, e);
                }
                let at = match renderers.iter().position(|r| r.0 == req.layers) {
                    Some(i) => i,
                    None => {
                        renderers
                            .push((req.layers.clone(), renderer_for(&rs, &kinds, &req.layers)));
                        renderers.len() - 1
                    }
                };
                let renderer = &renderers[at].1;
                // Never blocks on disk: tiles not loaded yet show a coarser level.
                // The lock covers taking the tiles; the copy into rasters runs without it.
                let (fetched, mark, seed, shader) =
                    match lock_project(&project, "render").0.as_ref() {
                        Some(p) => {
                            let marks = applied.lock().unwrap_or_else(|e| e.into_inner());
                            let mark = marks.get(&req.view.map).copied().unwrap_or_default();
                            let f = renderer
                                .fetch_cached(p, req.view)
                                .and_then(|f| f.with_summaries(p));
                            // The height pass of the shown map's kind draws its live copy.
                            let shader = p
                                .maps()
                                .iter()
                                .find(|m| m.id == req.view.map)
                                .and_then(|m| kinds.get(m.settings.kind))
                                .and_then(|k| {
                                    k.render_passes().iter().find(|r| r.layer == Layer::HEIGHT)
                                })
                                .map(|r| r.shader);
                            (f, mark, p.seed().unwrap_or(0), shader)
                        }
                        None => continue,
                    };
                let scene = fetched.map(|f| f.scene());
                let complete = scene.as_ref().is_ok_and(|s| s.is_complete());
                let (result, base) = match scene {
                    Ok(s) => {
                        let img = renderer.render(&s, RENDER_TILE);
                        let base = complete.then_some(shader).flatten().and_then(|shader| {
                            let base = s.into_base(Layer::HEIGHT)?;
                            Some(LiveSource {
                                base,
                                shader,
                                mark,
                                seed,
                            })
                        });
                        (img, base)
                    }
                    Err(e) => (Err(e), None),
                };
                if !complete && result.is_ok() {
                    pending = Some((req.clone(), epoch));
                }
                if done.send((req, epoch, complete, result, base)).is_err() {
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
            live,
        }
    }

    /// A part sent to the edit worker, for the live brush.
    pub fn live_part(&mut self, map: MapId, stroke: u64, first: usize, cmd: &mmc_core::Command) {
        if let mmc_core::Command::Brush(b) = cmd
            && b.layer == Layer::HEIGHT
        {
            self.live.part(map, stroke, first, b);
        }
    }

    /// A part of `stroke` failed on the CPU.
    pub fn live_failed(&mut self, map: MapId, stroke: u64) {
        self.live.failed(map, stroke);
    }

    /// True while the live brush shows strokes the canvas image does not have yet.
    pub fn live_showing(&self) -> bool {
        self.live.showing()
    }

    /// Forgets the shown image, for example after another project opens.
    pub fn clear(&mut self) {
        self.requested = None;
        self.shown = None;
        self.error = None;
        self.live.clear();
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
        while let Ok((v, epoch, complete, result, base)) = self.results.try_recv() {
            if complete {
                self.fresh_epoch = self.fresh_epoch.max(epoch);
            }
            // A render for another map or layer set is out of date.
            if v.view.map != req.view.map || v.layers != req.layers {
                continue;
            }
            if let Some(b) = base {
                self.live.base(b);
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
        // Strokes the image does not have yet, shaded from the GPU copy in this frame.
        if req.layers.contains(&Layer::HEIGHT)
            && let Some(cb) = self.live.frame(&view, rect, ppp)
        {
            ui.painter_at(rect).add(cb);
        }
    }
}
