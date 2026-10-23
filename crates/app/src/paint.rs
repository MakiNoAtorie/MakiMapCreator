//! Painting UI and state: a stroke goes to the edit worker in parts while the pen moves; each part shows as an overlay until drawn.

use std::time::Instant;

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use mmc_core::{MapId, RootPoint, Tool, ToolAction};

use crate::brush::{self, Direction, Edit, MAX_STRENGTH, Options, Sample};
use crate::canvas::Canvas;
use crate::edit::{Done, Editor, Op};
use crate::i18n::{tr, tr_args};
use crate::input::Phase;
use crate::theme::Theme;
use crate::view::MapView;

const TOOL_BUTTON: f32 = 32.0;

/// A pen or mouse sample on the canvas, in egui points.
pub struct Input {
    pub pos: Pos2,
    pub pressure: f32,
    pub phase: Phase,
}

/// The stroke the pen is drawing.
struct Live {
    id: u64,
    path: Vec<Sample>,
    /// Stamps already sent.
    sent: usize,
    parts: u32,
}

/// Stamps of one sent part, shown until a render requested after the part was applied arrives.
struct Pending {
    stroke: u64,
    part: u32,
    discs: Vec<(RootPoint, f64)>,
    raise: bool,
    /// The canvas epoch requested once the part was applied.
    epoch: Option<u64>,
}

pub struct Painter {
    map: MapId,
    /// The map kind's edit tools; `tool` indexes them.
    tools: &'static [Tool],
    pub tool: usize,
    pub dir: Direction,
    pub options: Options,
    mpp: f64,
    live: Option<Live>,
    next_stroke: u64,
    /// Ops for the edit worker, sent at the next `flush`.
    outbox: Vec<Op>,
    overlay: Vec<Pending>,
    /// Ended strokes whose overlay is still shown, with their pen-up time, for the log.
    drawing: Vec<(u64, Instant)>,
    /// Last known cursor and pressure, for the brush rings.
    cursor: Option<(Pos2, f32)>,
    /// Where the pen hovers in root space, and whether its tiles were prefetched.
    hover: Option<(RootPoint, bool)>,
    /// Bumped when an undo or redo changed the project; the app reloads what it shows.
    pub version: u64,
}

impl Painter {
    pub fn new(map: MapId, mpp: f64, tools: &'static [Tool]) -> Painter {
        Painter {
            map,
            tools,
            tool: 0,
            dir: Direction::Raise,
            options: Options::for_map(mpp),
            mpp,
            live: None,
            next_stroke: 0,
            outbox: vec![],
            overlay: vec![],
            drawing: vec![],
            cursor: None,
            hover: None,
            version: 0,
        }
    }

    fn current(&self) -> Option<Tool> {
        self.tools.get(self.tool).copied()
    }

    /// Notes a change made outside the painter (entity edit, generator) so the canvas redraws.
    pub fn changed(&mut self, canvas: &mut Canvas) {
        self.version += 1;
        canvas.invalidate();
    }

    pub fn edit(&mut self, e: Edit) {
        match e {
            Edit::Undo => self.outbox.push(Op::Undo(self.map)),
            Edit::Redo => self.outbox.push(Op::Redo(self.map)),
            Edit::SizeDown => self.options.step_size(-1, self.mpp),
            Edit::SizeUp => self.options.step_size(1, self.mpp),
            Edit::Direction(d) => self.dir = d,
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
            Phase::Hover => {
                if self.hover.is_none_or(|(at, _)| at != sample.at) {
                    self.hover = Some((sample.at, false));
                }
            }
            Phase::Down => {
                self.end();
                self.next_stroke += 1;
                self.live = Some(Live {
                    id: self.next_stroke,
                    path: vec![sample],
                    sent: 0,
                    parts: 0,
                });
            }
            Phase::Move => {
                if let Some(l) = &mut self.live {
                    l.path.push(sample);
                }
            }
            Phase::Up => {
                if let Some(l) = &mut self.live {
                    l.path.push(sample);
                }
                self.end();
            }
        }
    }

    /// Sends the stamps of the live stroke not sent yet as one part.
    fn send_part(&mut self) {
        let Some(t) = self.current() else { return };
        let Some(l) = &mut self.live else { return };
        let Some(cmd) = brush::plan(
            t.action,
            t.layer,
            self.dir,
            &self.options,
            self.mpp,
            &l.path,
        )
        .and_then(|c| brush::tail(c, l.sent)) else {
            return;
        };
        let discs = brush::stamp_discs(&cmd);
        l.sent += discs.len();
        l.parts += 1;
        self.overlay.push(Pending {
            stroke: l.id,
            part: l.parts,
            discs,
            raise: self.dir == Direction::Raise,
            epoch: None,
        });
        self.outbox.push(Op::Part {
            map: self.map,
            stroke: l.id,
            part: l.parts,
            cmd,
            sent: Instant::now(),
        });
    }

    /// Pen up: sends the rest of the live stroke and ends it.
    fn end(&mut self) {
        self.send_part();
        let Some(l) = self.live.take() else { return };
        let up = Instant::now();
        log::info!("stroke {}: {} samples", l.id, l.path.len());
        self.drawing.push((l.id, up));
        self.outbox.push(Op::End {
            map: self.map,
            stroke: l.id,
            up,
        });
    }

    /// Sends this frame's parts and ops to the edit worker; drops overlay parts the canvas now shows.
    pub fn flush(&mut self, editor: &Editor, canvas: &Canvas) {
        // A hovering pen will draw here: its tiles go into the cache first (ADR-0014).
        if let (Some((at, false)), Some(t)) = (self.hover, self.current()) {
            self.hover = Some((at, true));
            self.outbox.push(Op::Prefetch {
                map: self.map,
                layer: t.layer,
                at,
                radius: self.options.size * 1.25,
            });
        }
        self.send_part();
        for op in self.outbox.drain(..) {
            editor.send(op);
        }
        self.overlay
            .retain(|p| p.epoch.is_none_or(|e| !canvas.fresh(e)));
        let overlay = &self.overlay;
        self.drawing.retain(|&(id, up)| {
            let shown = !overlay.iter().any(|p| p.stroke == id);
            if shown {
                log::info!(
                    "stroke {id}: drawn {:.0} ms after pen up",
                    up.elapsed().as_secs_f64() * 1000.0
                );
            }
            !shown
        });
    }

    /// Takes one result of the edit worker for this map; gives an error text.
    pub fn done(&mut self, d: Done, canvas: &mut Canvas) -> Option<String> {
        let error = |e: &mmc_core::Error| {
            Some(tr_args(
                "error-paint",
                &[("error", &crate::errors::text(e))],
            ))
        };
        match d {
            Done::Applied {
                stroke,
                parts,
                result,
                ..
            } => {
                let mine = |p: &Pending| p.stroke == stroke && parts.contains(&p.part);
                match result {
                    Ok(()) => {
                        let epoch = canvas.invalidate();
                        for p in self.overlay.iter_mut().filter(|p| mine(p)) {
                            p.epoch = Some(epoch);
                        }
                        None
                    }
                    Err(e) => {
                        log::info!("stroke {stroke} parts {parts:?}: {e}");
                        self.overlay.retain(|p| !mine(p));
                        error(&e)
                    }
                }
            }
            Done::Ended { result, .. } => result.err().and_then(|e| error(&e)),
            Done::Stepped { result, .. } => match result {
                Ok(changed) => {
                    if changed {
                        self.version += 1;
                        canvas.invalidate();
                    }
                    None
                }
                Err(e) => error(&e),
            },
            Done::Rebuilt(r) => {
                canvas.invalidate();
                r.err().and_then(|e| error(&e))
            }
        }
    }

    pub fn hide_cursor(&mut self) {
        self.cursor = None;
    }

    /// Draws the strokes not yet on the canvas and the brush rings.
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
        for p in &self.overlay {
            for &(at, r) in &p.discs {
                painter.circle_filled(view.screen_at(rect, at), to_pt(r), tint(p.raise));
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
