//! The map canvas: pan and zoom, picking, pen and mouse painting, cursor readouts.

use eframe::egui::{self, Key, Sense};
use mmc_core::Layer;

use crate::i18n::tr_args;
use crate::input::Phase;
use crate::panels::Intent;
use crate::theme::Theme;
use crate::view::MapView;
use crate::{App, paint, workspace};

/// Zoom factor per scroll point.
const ZOOM_PER_POINT: f64 = 1.002;
/// How near the cursor a point or path must be to count as under it, in screen points.
const PICK_POINTS: f64 = 6.0;

impl App {
    pub(crate) fn map_canvas(
        &mut self,
        ui: &mut egui::Ui,
        pen_hover: Option<egui::Pos2>,
        intents: &mut Vec<Intent>,
    ) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let can_paint = self.can_paint();
        let Some(doc) = &self.doc else {
            return;
        };
        let (map, s) = (doc.map, doc.settings);
        let mut view = *self.view.get_or_insert_with(|| {
            let m = s.metres_per_pixel;
            MapView::fit(s.width as f64 * m, s.height as f64 * m, rect.shrink(16.0))
        });
        // Pan: middle drag, or Space and primary drag. Zoom: scroll wheel at the cursor.
        let space = ui.input(|i| i.key_down(Key::Space));
        if response.dragged_by(egui::PointerButton::Middle)
            || (space && response.dragged_by(egui::PointerButton::Primary))
        {
            view.pan(response.drag_delta());
        }
        if let Some(p) = response.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                view.zoom_at(rect, p, ZOOM_PER_POINT.powf(scroll as f64));
            }
        }
        self.view = Some(view);
        let (now, ppp) = ui.input(|i| (i.time, i.pixels_per_point));
        let cursor = pen_hover
            .or(response.hover_pos())
            .filter(|p| rect.contains(*p));
        // Click selects the place under the cursor; with a tool active it needs Ctrl.
        if response.clicked_by(egui::PointerButton::Primary)
            && (!can_paint || ui.input(|i| i.modifiers.command))
            && let Some(pos) = response.interact_pointer_pos()
        {
            intents.push(Intent::Select(workspace::pick(
                &self.snap.outlines,
                view.root_at(rect, pos),
            )));
        }
        if can_paint {
            self.paint_input(&response, rect, &view, cursor, space, ppp, now);
        } else {
            self.pen_events.clear();
            self.pen_down = false;
            self.mouse_down = false;
            self.painter(map).hide_cursor();
        }
        let hover = cursor.map(|p| view.root_at(rect, p));
        // Keep the last values while a worker holds the project (ADR-0014).
        if let Ok(guard) = self.project.try_lock()
            && let Some(p) = guard.as_ref()
        {
            self.elevation =
                hover.and_then(|r| p.layer_value(map, Layer::HEIGHT, r).ok().flatten());
            if hover != self.hover {
                let tolerance = PICK_POINTS * view.metres_per_point;
                self.hover_entity = hover
                    .and_then(|r| p.entity_at(map, r, tolerance).ok().flatten())
                    .and_then(|id| p.entity(id).ok().flatten())
                    .map(|e| e.name);
            }
        }
        self.hover = hover;
        let layers: Vec<Layer> = self
            .kinds
            .get(s.kind)
            .map(|k| {
                k.render_passes()
                    .iter()
                    .filter(|p| !self.hidden.contains(&p.layer))
                    .flat_map(|p| [Some(p.layer), p.relief])
                    .flatten()
                    .filter(|l| !self.hidden.contains(l))
                    .collect()
            })
            .unwrap_or_default();
        let theme = Theme::new(self.dark);
        let mut canvas = self.canvas.take();
        if let Some(c) = &mut canvas {
            for d in self.editor.done() {
                let target = d.map().unwrap_or(map);
                if let Some(m) = self.painter(target).done(d, c) {
                    self.message = Some(m);
                }
            }
            let editor = &self.editor;
            if let Some(p) = self.painters.get_mut(&map) {
                p.flush(editor, c);
            }
            c.show(ui, rect, map, layers, &view);
            if let Some(e) = &c.error {
                self.message = Some(tr_args("error-render", &[("error", e)]));
            }
        }
        let live = canvas.as_ref().is_some_and(|c| c.live_showing());
        self.canvas = canvas;
        self.painter(map).draw(ui, rect, &view, &theme, live);
        if let Some(sel) = self.selection
            && let Some((_, pts)) = self.snap.outlines.iter().find(|o| o.0 == sel)
        {
            let pts = pts.iter().map(|q| view.screen_at(rect, *q)).collect();
            ui.painter_at(rect).add(egui::Shape::closed_line(
                pts,
                egui::Stroke::new(2.0, theme.accent),
            ));
        }
    }

    /// Feeds this frame's pen events, or the mouse, to the painter of the shown map.
    #[allow(clippy::too_many_arguments)]
    fn paint_input(
        &mut self,
        response: &egui::Response,
        rect: egui::Rect,
        view: &MapView,
        cursor: Option<egui::Pos2>,
        space: bool,
        ppp: f32,
        now: f64,
    ) {
        let Some(map) = self.doc.as_ref().map(|d| d.map) else {
            return;
        };
        let events = std::mem::take(&mut self.pen_events);
        let mouse_ok = self.pen_filter.accept_mouse_press(now);
        let (mut pen_down, mut mouse_down) = (self.pen_down, self.mouse_down);
        let painter = self.painter(map);
        let mut sent = false;
        for e in events {
            let pos = egui::pos2(e.pos[0], e.pos[1]) / ppp;
            match e.phase {
                Phase::Down => pen_down = rect.contains(pos),
                Phase::Up | Phase::Move if !pen_down => continue,
                _ => {}
            }
            let input = paint::Input {
                pos,
                pressure: e.pressure,
                phase: e.phase,
            };
            painter.input(&input, view, rect);
            sent = true;
            if e.phase == Phase::Up {
                pen_down = false;
            }
        }
        if !space {
            let pos = response.interact_pointer_pos().or(cursor);
            let mouse = |phase| paint::Input {
                pos: pos.unwrap_or_default(),
                pressure: 1.0,
                phase,
            };
            if response.drag_started_by(egui::PointerButton::Primary) && mouse_ok {
                mouse_down = true;
                if pos.is_some() {
                    painter.input(&mouse(Phase::Down), view, rect);
                    sent = true;
                }
            } else if mouse_down && pos.is_some() {
                let stopped = response.drag_stopped();
                let phase = if stopped { Phase::Up } else { Phase::Move };
                painter.input(&mouse(phase), view, rect);
                sent = true;
                mouse_down = !stopped;
            }
        }
        if !sent {
            match cursor {
                Some(pos) => {
                    let hover = paint::Input {
                        pos,
                        pressure: 0.0,
                        phase: Phase::Hover,
                    };
                    painter.input(&hover, view, rect)
                }
                None => painter.hide_cursor(),
            }
        }
        (self.pen_down, self.mouse_down) = (pen_down, mouse_down);
    }
}
