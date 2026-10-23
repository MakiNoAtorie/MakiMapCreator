//! Live brush (ADR-0025): sent stroke parts show on a GPU copy until a complete render has them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use eframe::egui;
use eframe::egui_wgpu::{self, RenderState};
use mmc_core::{BrushStroke, MapId};
use mmc_render::{FlattenLevel, Live, LiveBase, LiveFrame, View};

/// The stroke the edit worker applies on a map and how far it is.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Mark {
    pub stroke: u64,
    /// Stamps of `stroke` in the project.
    pub stamps: usize,
    /// The stroke's flatten level, once its first stamp is in.
    pub level: Option<f32>,
}

/// Per map, written and read under the project lock, so a render knows which stamps it shows.
pub type Applied = Arc<Mutex<HashMap<MapId, Mark>>>;

/// A complete render's heights for the live brush.
pub struct LiveSource {
    pub base: LiveBase,
    /// The height pass of the map's kind.
    pub shader: &'static str,
    /// The stamps the render shows.
    pub mark: Mark,
    pub seed: u64,
}

/// A sent part of a stroke on `LiveBrush::map`: the index of its first stamp in the stroke, its stamps.
struct Part {
    stroke: u64,
    first: usize,
    cmd: BrushStroke,
}

/// Drops the parts, or their first stamps, that a render with `mark` already shows.
fn unapplied(parts: &mut Vec<Part>, mark: Mark) {
    parts.retain_mut(|p| {
        if p.stroke < mark.stroke {
            return false;
        }
        if p.stroke == mark.stroke {
            let done = mark.stamps.saturating_sub(p.first).min(p.cmd.points.len());
            p.cmd.points.drain(..done);
            p.first += done;
        }
        !p.cmd.points.is_empty()
    });
}

/// Where part `p`'s flatten level comes from, with `mark` the copy's base.
fn level(p: &Part, mark: Mark) -> FlattenLevel {
    match mark.level {
        Some(l) if mark.stroke == p.stroke => FlattenLevel::At(l),
        _ if p.first == 0 => FlattenLevel::Sample,
        _ => FlattenLevel::Kept,
    }
}

pub struct LiveBrush {
    rs: RenderState,
    /// The copy and the height pass it shades with.
    gpu: Option<(Live, &'static str)>,
    /// The map of `parts`.
    map: Option<MapId>,
    parts: Vec<Part>,
    /// The newest complete render, not uploaded yet.
    source: Option<LiveSource>,
    /// The mark and seed of the uploaded base.
    mark: Mark,
    seed: u64,
    /// The copy holds the newest base and every part.
    current: bool,
}

impl LiveBrush {
    pub fn new(rs: RenderState) -> LiveBrush {
        LiveBrush {
            rs,
            gpu: None,
            map: None,
            parts: vec![],
            source: None,
            mark: Mark::default(),
            seed: 0,
            current: false,
        }
    }

    pub fn clear(&mut self) {
        self.parts.clear();
        self.source = None;
        self.current = false;
        if let Some((g, _)) = &mut self.gpu {
            g.clear();
        }
    }

    /// A part sent to the edit worker; stamped on the copy now when it is current.
    pub fn part(&mut self, map: MapId, stroke: u64, first: usize, cmd: &BrushStroke) {
        if self.map != Some(map) {
            self.parts.clear();
            self.map = Some(map);
        }
        let part = Part {
            stroke,
            first,
            cmd: cmd.clone(),
        };
        let level = level(&part, self.mark);
        self.parts.push(part);
        if !self.current {
            self.upload();
            return;
        }
        let ok = self
            .gpu
            .as_mut()
            .is_some_and(|(g, _)| g.map() == Some(map) && g.stamp(self.seed, cmd, level));
        self.current = ok;
    }

    /// A part of `stroke` failed on the CPU: the copy no longer matches it.
    pub fn failed(&mut self, map: MapId, stroke: u64) {
        if self.map == Some(map) {
            self.parts.retain(|p| p.stroke != stroke);
        }
        self.current = false;
    }

    /// A complete render of `source`'s map.
    pub fn base(&mut self, source: LiveSource) {
        let map = source.base.map();
        if self.map != Some(map) {
            self.parts.clear();
            self.map = Some(map);
        }
        unapplied(&mut self.parts, source.mark);
        // A current copy on the same pixels is this base plus the parts left: no upload.
        let same = self
            .gpu
            .as_ref()
            .is_some_and(|(g, s)| *s == source.shader && g.same_area(&source.base));
        if self.current && same && !self.parts.is_empty() {
            return;
        }
        self.source = Some(source);
        self.current = false;
        if !self.parts.is_empty() {
            self.upload();
        }
    }

    /// Uploads the newest base and replays the parts on it.
    fn upload(&mut self) {
        // A base kept for later: a part sent before any render waits for the next one.
        if self
            .source
            .as_ref()
            .is_none_or(|s| Some(s.base.map()) != self.map)
        {
            return;
        }
        let Some(s) = self.source.take() else {
            return;
        };
        if self
            .gpu
            .as_ref()
            .is_none_or(|(_, shader)| *shader != s.shader)
        {
            let rs = &self.rs;
            let live = Live::new(
                rs.device.clone(),
                rs.queue.clone(),
                s.shader,
                rs.target_format,
            );
            self.gpu = Some((live, s.shader));
        }
        let Some((gpu, _)) = &mut self.gpu else {
            return;
        };
        gpu.set_base(s.base);
        (self.mark, self.seed) = (s.mark, s.seed);
        let mut ok = true;
        for p in &self.parts {
            ok &= gpu.stamp(s.seed, &p.cmd, level(p, s.mark));
        }
        self.current = ok;
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
        let f = self.gpu.as_ref()?.0.frame(view, min)?;
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

    fn mark(stroke: u64, stamps: usize) -> Mark {
        Mark {
            stroke,
            stamps,
            level: None,
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
        unapplied(&mut p, mark(1, 0));
        assert_eq!(left(&p), [(1, 0, 3), (1, 3, 2), (2, 0, 2)]);
        let mut p = sent();
        unapplied(&mut p, mark(1, 4));
        assert_eq!(left(&p), [(1, 4, 1), (2, 0, 2)]);
        let mut p = sent();
        unapplied(&mut p, mark(2, 2));
        assert!(p.is_empty());
    }

    #[test]
    fn a_flatten_part_takes_the_cpu_level_else_samples_at_the_stroke_start() {
        let known = Mark {
            level: Some(7.0),
            ..mark(1, 2)
        };
        assert_eq!(level(&part(1, 2, 1), known), FlattenLevel::At(7.0));
        assert_eq!(level(&part(2, 0, 1), known), FlattenLevel::Sample);
        assert_eq!(level(&part(2, 3, 1), known), FlattenLevel::Kept);
        assert_eq!(level(&part(1, 0, 1), mark(0, 0)), FlattenLevel::Sample);
    }
}
