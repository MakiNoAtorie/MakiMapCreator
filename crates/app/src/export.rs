//! File > Export PNG: a size chooser, then an offscreen render on a worker (#6).

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use eframe::egui;
use mmc_core::{MapId, MapSettings, RootPoint};
use mmc_render::{Renderer, View};

use crate::i18n::{tr, tr_args};
use crate::{App, errors};

/// Largest export side in pixels; the image is held in memory (4 bytes a pixel).
const MAX_SIDE: u32 = 16384;
/// Render target tile side; any size gives the same image.
const TILE: u32 = 2048;

#[derive(Default)]
pub struct Export {
    /// The open size chooser: the map and the chosen width in pixels.
    dialog: Option<(MapSettings, u32)>,
    running: Option<JoinHandle<mmc_render::Result<PathBuf>>>,
}

impl Export {
    pub fn open(&mut self, settings: MapSettings) {
        self.dialog = Some((settings, settings.width.min(MAX_SIDE)));
    }

    pub fn busy(&self) -> bool {
        self.running.is_some()
    }
}

/// Image height for `width` that keeps the map's aspect.
fn height_for(s: &MapSettings, width: u32) -> u32 {
    ((width as f64 * s.height as f64 / s.width as f64).round() as u32).clamp(1, MAX_SIDE)
}

impl App {
    /// The size chooser window; Export asks for a file and starts the worker.
    pub(crate) fn export_dialog(&mut self, ctx: &egui::Context) {
        let Some((s, width)) = self.export.dialog.as_mut() else {
            return;
        };
        let (mut go, mut cancel) = (false, false);
        egui::Window::new(tr("export-title"))
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr("export-width"));
                    ui.add(
                        egui::DragValue::new(width)
                            .range(1..=MAX_SIDE)
                            .suffix(" px"),
                    );
                });
                let h = height_for(s, *width).to_string();
                ui.weak(tr_args("export-height", &[("h", &h)]));
                ui.horizontal(|ui| {
                    go = ui.button(tr("export-run")).clicked();
                    cancel = ui.button(tr("export-cancel")).clicked();
                });
            });
        if cancel || go {
            let (s, width) = self.export.dialog.take().expect("dialog is open");
            if go
                && let Some(file) = rfd::FileDialog::new()
                    .add_filter(tr("export-filter"), &["png"])
                    .save_file()
            {
                self.start_export(s, width, file.with_extension("png"));
            }
        }
    }

    fn start_export(&mut self, s: MapSettings, width: u32, file: PathBuf) {
        let (Some(rs), Some(map)) = (self.render_state.clone(), self.doc.as_ref().map(|d| d.map))
        else {
            return;
        };
        let (project, kinds) = (self.project.clone(), self.kinds.clone());
        let hidden = self.hidden.clone();
        self.export.running = Some(std::thread::spawn(move || {
            let mut r = Renderer::from_device(rs.device.clone(), rs.queue.clone());
            for p in kinds.get(s.kind).map_or(&[][..], |k| k.render_passes()) {
                if !hidden.contains(&p.layer) {
                    let pass = mmc_render::ShaderPass::new(r.device(), p.shader);
                    r.register(p.layer, Box::new(pass));
                }
            }
            let view = full_view(map, &s, width);
            // Blocking reads at full resolution, then the render without the project lock.
            let scene = match project.lock().unwrap().as_ref() {
                Some(p) => r.scene(p, view).and_then(|sc| sc.with_summaries(p))?,
                None => return Err(mmc_render::Error::OutsideMap),
            };
            mmc_render::export_png(Arc::new(r), scene, TILE, file.clone())
                .join()
                .unwrap_or_else(|_| Err(mmc_render::Error::Gpu("export worker stopped".into())))?;
            Ok(file)
        }));
    }

    pub(crate) fn finish_export(&mut self) {
        if !self
            .export
            .running
            .as_ref()
            .is_some_and(|j| j.is_finished())
        {
            return;
        }
        let Some(job) = self.export.running.take() else {
            return;
        };
        self.message = Some(match job.join() {
            Ok(Ok(file)) => tr_args("export-done", &[("file", &file.display().to_string())]),
            Ok(Err(e)) => tr_args("error-export", &[("error", &errors::render_text(&e))]),
            Err(_) => tr_args("error-export", &[("error", &tr("error-crashed"))]),
        });
    }
}

/// The whole map at `width` pixels wide.
fn full_view(map: MapId, s: &MapSettings, width: u32) -> View {
    View {
        map,
        min: RootPoint { x: 0.0, y: 0.0 },
        max: RootPoint {
            x: s.width as f64 * s.metres_per_pixel,
            y: s.height as f64 * s.metres_per_pixel,
        },
        width,
        height: height_for(s, width),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_keeps_the_map_aspect() {
        let s = MapSettings {
            kind: mmc_core::MapKind::new("region"),
            width: 2000,
            height: 1000,
            metres_per_pixel: 10.0,
        };
        assert_eq!(height_for(&s, 4000), 2000);
        assert_eq!(height_for(&s, 1), 1);
    }
}
