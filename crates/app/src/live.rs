//! Live brush (ADR-0025): sent stroke parts go on a GPU copy of the drawn heights in the frame
//! they are made, until a complete CPU render includes them. The CPU stroke stays the truth.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use eframe::egui;
use eframe::egui_wgpu::{self, RenderState};
use mmc_core::{BrushStroke, MapId};
use mmc_render::{Live, LiveBase, LiveFrame, View};

/// Per map, the stroke the edit worker applies and how many of its stamps are in the project.
/// Written and read under the project lock, so a render knows which stamps it shows.
pub type Applied = Arc<Mutex<HashMap<MapId, (u64, usize)>>>;

/// A sent part of a stroke on `LiveBrush::map`: the index of its first stamp in the stroke, its stamps.
struct Part {
    stroke: u64,
    first: usize,
    cmd: BrushStroke,
}

/// Drops the parts, or their first stamps, that a render with `applied` already shows.
fn unapplied(parts: &mut Vec<Part>, (stroke, n): (u64, usize)) {
    parts.retain_mut(|p| {
        if p.stroke < stroke {
            return false;
        }
        if p.stroke == stroke {
            let done = n.saturating_sub(p.first).min(p.cmd.points.len());
            p.cmd.points.drain(..done);
            p.first += done;
        }
        !p.cmd.points.is_empty()
    });
}

pub struct LiveBrush {
    rs: RenderState,
    shader: &'static str,
    gpu: Option<Live>,
    /// The map of `parts`.
    map: Option<MapId>,
    parts: Vec<Part>,
    /// The newest complete render's heights, applied mark and project seed, not uploaded yet.
    base: Option<(LiveBase, u64)>,
    seed: u64,
    /// The copy holds the newest base and every part.
    current: bool,
}

impl LiveBrush {
    /// `shader` is the height pass of the shown map's kind.
    pub fn new(rs: RenderState, shader: &'static str) -> LiveBrush {
        LiveBrush {
            rs,
            shader,
            gpu: None,
            map: None,
            parts: vec![],
            base: None,
            seed: 0,
            current: false,
        }
    }

    pub fn clear(&mut self) {
        self.parts.clear();
        self.base = None;
        self.current = false;
        if let Some(g) = &mut self.gpu {
            g.clear();
        }
    }

    /// A part sent to the edit worker; stamped on the copy now when it is current.
    pub fn part(&mut self, map: MapId, stroke: u64, first: usize, cmd: &BrushStroke) {
        if self.map != Some(map) {
            self.parts.clear();
            self.map = Some(map);
        }
        self.parts.push(Part {
            stroke,
            first,
            cmd: cmd.clone(),
        });
        if !self.current {
            self.upload();
            return;
        }
        let ok = self
            .gpu
            .as_mut()
            .is_some_and(|g| g.map() == Some(map) && g.stamp(self.seed, cmd));
        self.current = ok;
    }

    /// A part of `stroke` failed on the CPU: the copy no longer matches it.
    pub fn failed(&mut self, map: MapId, stroke: u64) {
        if self.map == Some(map) {
            self.parts.retain(|p| p.stroke != stroke);
        }
        self.current = false;
    }

    /// A complete render of `base`'s map that shows the stamps in `applied`.
    pub fn base(&mut self, base: LiveBase, applied: (u64, usize), seed: u64) {
        if self.map != Some(base.map()) {
            self.parts.clear();
            self.map = Some(base.map());
        }
        unapplied(&mut self.parts, applied);
        // A current copy on the same pixels is this base plus the parts left: no upload.
        let same = self.gpu.as_ref().is_some_and(|g| g.same_area(&base));
        if self.current && same && !self.parts.is_empty() {
            return;
        }
        self.base = Some((base, seed));
        self.current = false;
        if !self.parts.is_empty() {
            self.upload();
        }
    }

    /// Uploads the newest base and replays the parts on it.
    fn upload(&mut self) {
        // A base kept for later: a part on the CPU path waits for the next render.
        let map = self.map;
        if self.base.as_ref().is_none_or(|(b, _)| Some(b.map()) != map)
            || !self.parts.iter().all(|p| Live::supports(&p.cmd))
        {
            return;
        }
        let Some((base, seed)) = self.base.take() else {
            return;
        };
        let rs = &self.rs;
        let shader = self.shader;
        let gpu = self.gpu.get_or_insert_with(|| {
            Live::new(
                rs.device.clone(),
                rs.queue.clone(),
                shader,
                rs.target_format,
            )
        });
        gpu.set_base(base);
        self.seed = seed;
        for p in &self.parts {
            gpu.stamp(seed, &p.cmd);
        }
        self.current = true;
    }

    /// True while the copy stands in for the canvas image.
    pub fn showing(&self) -> bool {
        self.current && !self.parts.is_empty()
    }

    /// The draw of the copy for `view` shown in `rect`, `ppp` physical pixels per point.
    pub fn frame(&self, view: &View, rect: egui::Rect, ppp: f32) -> Option<egui::PaintCallback> {
        if !self.showing() {
            return None;
        }
        let min = [-rect.min.x * ppp, -rect.min.y * ppp];
        let f = self.gpu.as_ref()?.frame(view, min)?;
        Some(egui_wgpu::Callback::new_paint_callback(rect, Draw(f)))
    }
}

struct Draw(LiveFrame);

impl egui_wgpu::CallbackTrait for Draw {
    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut eframe::wgpu::RenderPass<'static>,
        _resources: &egui_wgpu::CallbackResources,
    ) {
        self.0.draw(pass);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mmc_core::{RootPoint, StrokePoint};

    fn part(stroke: u64, first: usize, n: usize) -> Part {
        let point = StrokePoint {
            at: RootPoint { x: 0.0, y: 0.0 },
            pressure: 1.0,
            radius: 1.0,
        };
        Part {
            stroke,
            first,
            cmd: BrushStroke {
                points: vec![point; n],
                ..Default::default()
            },
        }
    }

    fn left(parts: &[Part]) -> Vec<(u64, usize, usize)> {
        parts
            .iter()
            .map(|p| (p.stroke, p.first, p.cmd.points.len()))
            .collect()
    }

    #[test]
    fn a_render_drops_the_stamps_it_shows() {
        let sent = || vec![part(1, 0, 3), part(1, 3, 2), part(2, 0, 2)];
        let mut p = sent();
        unapplied(&mut p, (1, 0));
        assert_eq!(left(&p), [(1, 0, 3), (1, 3, 2), (2, 0, 2)]);
        let mut p = sent();
        unapplied(&mut p, (1, 4));
        assert_eq!(left(&p), [(1, 4, 1), (2, 0, 2)]);
        let mut p = sent();
        unapplied(&mut p, (2, 2));
        assert!(p.is_empty());
    }
}
