//! Painting state and UI: tool strip, options bar, brush cursor, stroke overlay, undo groups.
//!
//! A stroke is shown by an overlay while the pen is down and until the renderer shows the
//! edit; core gets one set of commands at pen up so a stroke is one undo step.

use std::collections::VecDeque;

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use mmc_core::{BrushStroke, MapId, RootPoint};

use crate::brush::{self, Edit, MAX_STRENGTH, Options, Sample, Tool};
use crate::canvas::{Canvas, SharedProject};
use crate::i18n::{tr, tr_args};
use crate::input::Phase;
use crate::theme::Theme;
use crate::view::MapView;

/// Seconds the app waits for derived layers after an edit.
const JOBS_WAIT: f64 = 30.0;
/// The fixed core profile `(1-d^2)^2` is half its peak at this fraction of the radius.
const HALF_RADIUS: f32 = 0.541;
const TOOL_BUTTON: f32 = 32.0;

enum Op {
    Commit(Vec<BrushStroke>),
    Undo,
    Redo,
}

/// A pen or mouse sample on the canvas, in egui points.
pub struct Input {
    pub pos: Pos2,
    pub pressure: f32,
    pub phase: Phase,
}

pub struct Painter {
    pub tool: Tool,
    pub options: Options,
    mpp: f64,
    stroke: Vec<Sample>,
    ops: VecDeque<Op>,
    /// Stroke stamps shown until the renderer catches up: (centre, radius m, raise).
    overlay: Vec<(RootPoint, f64, bool)>,
    await_epoch: Option<u64>,
    jobs_until: Option<f64>,
    /// Core commands per stroke; undo and redo move whole strokes.
    // ponytail: assumes only strokes are applied on the map; entity edits (#10) need a shared group in core.
    done: Vec<usize>,
    undone: Vec<usize>,
    /// Last known cursor and pressure, for the brush rings.
    cursor: Option<(Pos2, f32)>,
    /// Bumped when an edit, undo or redo changed the project; the app reloads what it shows.
    pub version: u64,
}

impl Painter {
    pub fn new(mpp: f64) -> Painter {
        Painter {
            tool: Tool::Raise,
            options: Options::for_map(mpp),
            mpp,
            stroke: vec![],
            ops: VecDeque::new(),
            overlay: vec![],
            await_epoch: None,
            jobs_until: None,
            done: vec![],
            undone: vec![],
            cursor: None,
            version: 0,
        }
    }

    /// Records a command applied outside the brush (entity edit, generator) as one undo step.
    pub fn external_command(&mut self, canvas: &mut Canvas, now: f64, derived: bool) {
        self.done.push(1);
        self.undone.clear();
        self.version += 1;
        self.await_epoch = Some(canvas.invalidate());
        if derived {
            self.jobs_until = Some(now + JOBS_WAIT);
        }
    }

    pub fn edit(&mut self, e: Edit) {
        match e {
            Edit::Undo => self.ops.push_back(Op::Undo),
            Edit::Redo => self.ops.push_back(Op::Redo),
            Edit::SizeDown => self.options.step_size(-1, self.mpp),
            Edit::SizeUp => self.options.step_size(1, self.mpp),
            Edit::Tool(t) => self.tool = t,
        }
    }

    /// Feeds one sample from the canvas; `view` and `rect` map points to root space.
    pub fn input(&mut self, i: &Input, view: &MapView, rect: Rect) {
        self.cursor = (i.phase != Phase::Up).then_some((i.pos, i.pressure));
        let sample = Sample {
            at: view.root_at(rect, i.pos),
            pressure: i.pressure,
        };
        match i.phase {
            Phase::Hover => {}
            Phase::Down => self.stroke = vec![sample],
            Phase::Move => {
                if !self.stroke.is_empty() {
                    self.stroke.push(sample);
                }
            }
            Phase::Up => {
                if self.stroke.is_empty() {
                    return;
                }
                self.stroke.push(sample);
                let path = std::mem::take(&mut self.stroke);
                let strokes = brush::plan(self.tool, &self.options, self.mpp, &path);
                let raise = self.tool == Tool::Raise;
                self.overlay
                    .extend(stamps(&strokes).map(|(at, r)| (at, r, raise)));
                self.ops.push_back(Op::Commit(strokes));
            }
        }
    }

    /// Applies queued edits when the project is free, polls derived layers, and clears the
    /// overlay once the canvas shows the result. Returns an error text.
    pub fn flush(
        &mut self,
        project: &SharedProject,
        map: MapId,
        canvas: &mut Canvas,
        ctx: &egui::Context,
        now: f64,
    ) -> Option<String> {
        if let Some(e) = self.await_epoch
            && canvas.fresh(e)
        {
            self.await_epoch = None;
            self.overlay.clear();
        }
        if self.ops.is_empty() && self.jobs_until.is_none() {
            return None;
        }
        // The renderer or a save may hold the project; try again next frame.
        let Ok(mut guard) = project.try_lock() else {
            ctx.request_repaint();
            return None;
        };
        let p = guard.as_mut()?;
        let mut error = None;
        let mut changed = false;
        while let Some(op) = self.ops.pop_front() {
            let r = match op {
                Op::Commit(strokes) => {
                    let mut n = 0;
                    let mut r = Ok(());
                    for s in strokes {
                        r = p.apply(map, mmc_core::Command::Brush(s));
                        if r.is_err() {
                            break;
                        }
                        n += 1;
                    }
                    if n > 0 {
                        self.done.push(n);
                        self.undone.clear();
                    }
                    changed |= n > 0;
                    r
                }
                Op::Undo => step(p, map, &mut self.done, &mut self.undone, true, &mut changed),
                Op::Redo => step(
                    p,
                    map,
                    &mut self.undone,
                    &mut self.done,
                    false,
                    &mut changed,
                ),
            };
            if let Err(e) = r {
                error = Some(tr_args("error-paint", &[("error", &e.to_string())]));
            }
        }
        if changed {
            self.version += 1;
            self.await_epoch = Some(canvas.invalidate());
            self.jobs_until = Some(now + JOBS_WAIT);
        }
        if let Some(until) = self.jobs_until {
            match p.poll_jobs() {
                Ok(true) => {
                    canvas.invalidate();
                    self.jobs_until = None;
                }
                Ok(false) if now < until => ctx.request_repaint_after_secs(0.1),
                Ok(false) => self.jobs_until = None,
                Err(e) => {
                    self.jobs_until = None;
                    error = Some(tr_args("error-paint", &[("error", &e.to_string())]));
                }
            }
        }
        ctx.request_repaint();
        error
    }

    pub fn hide_cursor(&mut self) {
        self.cursor = None;
    }

    /// Draws the live stroke, the pending overlay and the brush rings over the canvas.
    pub fn draw(&self, ui: &egui::Ui, rect: Rect, view: &MapView, t: &Theme) {
        let painter = ui.painter_at(rect);
        let to_pt = |m: f64| (m / view.metres_per_point) as f32;
        let tint = |raise: bool| {
            if raise {
                Color32::from_rgba_unmultiplied(236, 220, 180, 28)
            } else {
                Color32::from_rgba_unmultiplied(20, 16, 12, 28)
            }
        };
        for &(at, r, raise) in &self.overlay {
            painter.circle_filled(view.screen_at(rect, at), to_pt(r), tint(raise));
        }
        if !self.stroke.is_empty() {
            let live = brush::plan(self.tool, &self.options, self.mpp, &self.stroke);
            let raise = self.tool == Tool::Raise;
            for (at, r) in stamps(&live) {
                painter.circle_filled(view.screen_at(rect, at), to_pt(r), tint(raise));
            }
        }
        if let Some((pos, pressure)) = self.cursor.filter(|(p, _)| rect.contains(*p)) {
            let r = to_pt(self.options.radius_at(pressure));
            painter.circle_stroke(pos, r, Stroke::new(1.0, t.text));
            painter.circle_stroke(pos, r * HALF_RADIUS, Stroke::new(1.0, t.text_weak));
        }
    }

    /// Left tool strip.
    pub fn tool_strip(&mut self, ui: &mut egui::Ui, t: &Theme) {
        for (tool, name, key) in [
            (Tool::Raise, "tool-raise", "R"),
            (Tool::Lower, "tool-lower", "L"),
        ] {
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(TOOL_BUTTON), Sense::click());
            let active = self.tool == tool;
            let fill = if active {
                t.accent_dim
            } else if resp.hovered() {
                t.widget_hi
            } else {
                Color32::TRANSPARENT
            };
            let painter = ui.painter();
            painter.rect_filled(rect, 2.0, fill);
            let colour = if active { t.accent } else { t.text };
            icon(painter, rect, tool == Tool::Raise, Stroke::new(1.5, colour));
            if resp
                .on_hover_text(tr_args("tool-tip", &[("name", &tr(name)), ("key", key)]))
                .clicked()
            {
                self.tool = tool;
            }
        }
    }

    /// Options bar for the brush tools.
    pub fn options_bar(&mut self, ui: &mut egui::Ui) {
        let mpp = self.mpp;
        let (lo, hi) = Options::size_limits(mpp);
        ui.horizontal(|ui| {
            ui.label(tr("options-size"));
            // The slider shows pixels; the option is metres.
            let mut px = self.options.size / mpp;
            ui.add(
                egui::Slider::new(&mut px, lo / mpp..=hi / mpp)
                    .logarithmic(true)
                    .suffix(" px"),
            );
            self.options.size = px * mpp;
            ui.separator();
            ui.label(tr("options-strength"));
            ui.add(egui::Slider::new(
                &mut self.options.strength,
                0.0..=MAX_STRENGTH,
            ));
            ui.separator();
            ui.label(tr("options-falloff"));
            ui.add_enabled(
                false,
                egui::Slider::new(&mut self.options.falloff, 0.0..=1.0),
            )
            .on_disabled_hover_text(tr("options-falloff-fixed"));
            ui.separator();
            ui.checkbox(&mut self.options.pressure_size, tr("options-pressure-size"));
            ui.checkbox(
                &mut self.options.pressure_strength,
                tr("options-pressure-strength"),
            );
        });
    }
}

fn step(
    p: &mut mmc_core::Project,
    map: MapId,
    from: &mut Vec<usize>,
    to: &mut Vec<usize>,
    undo: bool,
    changed: &mut bool,
) -> mmc_core::Result<()> {
    let Some(n) = from.pop() else {
        return Ok(());
    };
    for k in 0..n {
        let r = if undo { p.undo(map) } else { p.redo(map) };
        if let Err(e) = r {
            // Keep the strokes that did not move on the source stack.
            from.push(n - k);
            if k > 0 {
                to.push(k);
                *changed = true;
            }
            return Err(e);
        }
    }
    to.push(n);
    *changed = true;
    Ok(())
}

fn stamps(strokes: &[BrushStroke]) -> impl Iterator<Item = (RootPoint, f64)> + '_ {
    strokes
        .iter()
        .flat_map(|s| s.points.iter().map(|p| (p.at, s.radius)))
}

/// Raise: hill with an up arrow. Lower: valley with a down arrow.
fn icon(painter: &egui::Painter, rect: Rect, raise: bool, stroke: Stroke) {
    let at = |x: f32, y: f32| rect.min + Vec2::new(x, y) * rect.size();
    let line = |pts: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            pts.iter().map(|&(x, y)| at(x, y)).collect(),
            stroke,
        ));
    };
    if raise {
        line(&[
            (0.12, 0.85),
            (0.35, 0.55),
            (0.52, 0.72),
            (0.70, 0.48),
            (0.88, 0.85),
        ]);
        line(&[(0.5, 0.45), (0.5, 0.12)]);
        line(&[(0.38, 0.24), (0.5, 0.12), (0.62, 0.24)]);
    } else {
        line(&[
            (0.12, 0.15),
            (0.35, 0.45),
            (0.52, 0.28),
            (0.70, 0.52),
            (0.88, 0.15),
        ]);
        line(&[(0.5, 0.55), (0.5, 0.88)]);
        line(&[(0.38, 0.76), (0.5, 0.88), (0.62, 0.76)]);
    }
}
