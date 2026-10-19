//! Painting UI and state; one stroke is one command at pen up, shown by an overlay until drawn.

use std::collections::VecDeque;

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use mmc_core::{Command, MapId, RootPoint, Tool, ToolAction};

use crate::brush::{self, Direction, Edit, MAX_STRENGTH, Options, Sample};
use crate::canvas::{Canvas, SharedProject};
use crate::i18n::{tr, tr_args};
use crate::input::Phase;
use crate::theme::Theme;
use crate::view::MapView;

/// Seconds the app waits for derived layers after an edit.
const JOBS_WAIT: f64 = 30.0;
const TOOL_BUTTON: f32 = 32.0;

enum Op {
    Commit(Command),
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
    /// The map kind's edit tools; `tool` indexes them.
    tools: &'static [Tool],
    pub tool: usize,
    pub dir: Direction,
    pub options: Options,
    mpp: f64,
    stroke: Vec<Sample>,
    ops: VecDeque<Op>,
    /// Stroke stamps shown until the renderer catches up: (centre, radius m, raise).
    overlay: Vec<(RootPoint, f64, bool)>,
    await_epoch: Option<u64>,
    jobs_until: Option<f64>,
    /// Last known cursor and pressure, for the brush rings.
    cursor: Option<(Pos2, f32)>,
    /// Bumped when an edit, undo or redo changed the project; the app reloads what it shows.
    pub version: u64,
}

impl Painter {
    pub fn new(mpp: f64, tools: &'static [Tool]) -> Painter {
        Painter {
            tools,
            tool: 0,
            dir: Direction::Raise,
            options: Options::for_map(mpp),
            mpp,
            stroke: vec![],
            ops: VecDeque::new(),
            overlay: vec![],
            await_epoch: None,
            jobs_until: None,
            cursor: None,
            version: 0,
        }
    }

    fn current(&self) -> Option<Tool> {
        self.tools.get(self.tool).copied()
    }

    /// Notes a change made outside the painter (entity edit, generator) so the canvas redraws.
    pub fn changed(&mut self, canvas: &mut Canvas, now: f64, derived: bool) {
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
            Edit::Direction(d) => self.dir = d,
        }
    }

    fn plan(&self, path: &[Sample]) -> Option<Command> {
        let t = self.current()?;
        brush::plan(t.action, t.layer, self.dir, &self.options, self.mpp, path)
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
                if let Some(cmd) = self.plan(&path) {
                    let raise = self.dir == Direction::Raise;
                    let discs = brush::stamp_discs(&cmd);
                    self.overlay
                        .extend(discs.into_iter().map(|(at, r)| (at, r, raise)));
                    self.ops.push_back(Op::Commit(cmd));
                }
            }
        }
    }

    /// Applies queued edits, polls derived layers and drops the overlay once drawn; gives an error text.
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
                Op::Commit(cmd) => p.apply(map, cmd).map(|()| true),
                Op::Undo => p.undo(map),
                Op::Redo => p.redo(map),
            };
            match r {
                Ok(c) => changed |= c,
                Err(e) => {
                    error = Some(tr_args(
                        "error-paint",
                        &[("error", &crate::errors::text(&e))],
                    ))
                }
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
                    error = Some(tr_args(
                        "error-paint",
                        &[("error", &crate::errors::text(&e))],
                    ));
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
        if !self.stroke.is_empty()
            && let Some(live) = self.plan(&self.stroke)
        {
            let raise = self.dir == Direction::Raise;
            for (at, r) in brush::stamp_discs(&live) {
                painter.circle_filled(view.screen_at(rect, at), to_pt(r), tint(raise));
            }
        }
        if let Some((pos, pressure)) = self.cursor.filter(|(p, _)| rect.contains(*p)) {
            let r = to_pt(self.options.radius_at(pressure));
            painter.circle_stroke(pos, r, Stroke::new(1.0, t.text));
            let soft = self
                .current()
                .is_some_and(|t| t.action == ToolAction::Brush);
            if soft {
                let half = r * self.options.half_radius();
                painter.circle_stroke(pos, half, Stroke::new(1.0, t.text_weak));
            }
        }
    }

    /// Left tool strip: the kind's tools, then raise and lower.
    pub fn tool_strip(&mut self, ui: &mut egui::Ui, t: &Theme) {
        let button = |ui: &mut egui::Ui,
                      active: bool,
                      tip: String,
                      draw: &dyn Fn(&egui::Painter, Rect, Stroke)| {
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(TOOL_BUTTON), Sense::click());
            let fill = if active {
                t.accent_dim
            } else if resp.hovered() {
                t.widget_hi
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect, 2.0, fill);
            let colour = if active { t.accent } else { t.text };
            draw(ui.painter(), rect, Stroke::new(1.5, colour));
            resp.on_hover_text(tip).clicked()
        };
        for (i, tool) in self.tools.iter().enumerate() {
            let name = tr(&format!("tool-{}", tool.id));
            let brush = tool.action == ToolAction::Brush;
            let draw = move |p: &egui::Painter, r: Rect, s: Stroke| tool_icon(p, r, brush, s);
            if button(ui, self.tool == i, name, &draw) {
                self.tool = i;
            }
        }
        ui.separator();
        for (dir, name, key) in [
            (Direction::Raise, "tool-raise", "R"),
            (Direction::Lower, "tool-lower", "L"),
        ] {
            let tip = tr_args("tool-tip", &[("name", &tr(name)), ("key", key)]);
            let up = dir == Direction::Raise;
            let draw = move |p: &egui::Painter, r: Rect, s: Stroke| arrow(p, r, up, s);
            if button(ui, self.dir == dir, tip, &draw) {
                self.dir = dir;
            }
        }
    }

    /// Options bar for the active tool.
    pub fn options_bar(&mut self, ui: &mut egui::Ui) {
        let mpp = self.mpp;
        let (lo, hi) = Options::size_limits(mpp);
        let brush = self
            .current()
            .is_some_and(|t| t.action == ToolAction::Brush);
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
            if !brush {
                return;
            }
            ui.separator();
            ui.label(tr("options-strength"));
            ui.add(egui::Slider::new(
                &mut self.options.strength,
                0.0..=MAX_STRENGTH,
            ));
            ui.separator();
            ui.label(tr("options-falloff"));
            ui.add(egui::Slider::new(&mut self.options.falloff, 0.0..=1.0));
            ui.separator();
            ui.checkbox(&mut self.options.pressure_size, tr("options-pressure-size"));
            ui.checkbox(
                &mut self.options.pressure_strength,
                tr("options-pressure-strength"),
            );
        });
    }
}

/// A brush tool: a hill. A paint tool: a filled disc.
fn tool_icon(painter: &egui::Painter, rect: Rect, brush: bool, stroke: Stroke) {
    let at = |x: f32, y: f32| rect.min + Vec2::new(x, y) * rect.size();
    if brush {
        let pts = [
            (0.12, 0.8),
            (0.35, 0.45),
            (0.52, 0.62),
            (0.70, 0.35),
            (0.88, 0.8),
        ];
        painter.add(egui::Shape::line(
            pts.iter().map(|&(x, y)| at(x, y)).collect(),
            stroke,
        ));
    } else {
        painter.circle_filled(rect.center(), rect.width() * 0.25, stroke.color);
    }
}

/// Raise: an up arrow. Lower: a down arrow.
fn arrow(painter: &egui::Painter, rect: Rect, up: bool, stroke: Stroke) {
    let at = |x: f32, y: f32| rect.min + Vec2::new(x, y) * rect.size();
    let (tip, tail, wing) = if up {
        (0.2, 0.8, 0.38)
    } else {
        (0.8, 0.2, 0.62)
    };
    painter.line_segment([at(0.5, tail), at(0.5, tip)], stroke);
    painter.line_segment([at(0.35, wing), at(0.5, tip)], stroke);
    painter.line_segment([at(0.65, wing), at(0.5, tip)], stroke);
}
