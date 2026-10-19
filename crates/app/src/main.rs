mod input;
#[cfg(windows)]
mod pen_win;

use eframe::egui::{self, Color32, FontId, Pos2, Sense};
use input::{DuplicatePressFilter, PenApi, PenEvent, Phase};
use std::collections::HashSet;

/// Pen API setting. No settings UI yet; `cargo run -p mmc-app -- ink` overrides it.
const PEN_API: PenApi = PenApi::WinTab;
/// Seconds after a pen event during which a mouse press counts as the pen's duplicate.
const DUPLICATE_PRESS_WINDOW: f64 = 1.0;

fn main() -> eframe::Result {
    let setting = match std::env::args().nth(1).as_deref() {
        Some("ink") => PenApi::Ink,
        _ => PEN_API,
    };
    eframe::run_native(
        "MakiMapCreator",
        eframe::NativeOptions::default(),
        Box::new(move |cc| Ok(Box::new(App::new(cc, setting)))),
    )
}

struct App {
    api: String,
    last: Option<PenEvent>,
    levels: HashSet<u32>,
    dots: Vec<(Pos2, f32, Color32)>,
    filter: DuplicatePressFilter,
    mouse_accepted: u32,
    mouse_dropped: u32,
}

impl App {
    fn new(cc: &eframe::CreationContext, setting: PenApi) -> Self {
        Self {
            api: start_pen(cc, setting),
            last: None,
            levels: HashSet::new(),
            dots: Vec::new(),
            filter: DuplicatePressFilter::new(DUPLICATE_PRESS_WINDOW),
            mouse_accepted: 0,
            mouse_dropped: 0,
        }
    }
}

#[cfg(windows)]
fn start_pen(cc: &eframe::CreationContext, setting: PenApi) -> String {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(RawWindowHandle::Win32(h)) = cc.window_handle().map(|h| h.as_raw()) else {
        return "none (no Win32 window handle)".into();
    };
    let s = pen_win::install(h.hwnd.get(), setting, cc.egui_ctx.clone());
    format!("{:?} (setting {setting:?}; WinTab: {})", s.api, s.wintab)
}

#[cfg(not(windows))]
fn start_pen(_: &eframe::CreationContext, _: PenApi) -> String {
    "none (pen input is Windows only)".into()
}

#[cfg(windows)]
fn drain_pen() -> Vec<PenEvent> {
    pen_win::drain()
}

#[cfg(not(windows))]
fn drain_pen() -> Vec<PenEvent> {
    Vec::new()
}

impl eframe::App for App {
    // ponytail: debug overlay strings are literal; they move to the string lookup (ADR-0017) with the real canvas.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let (now, ppp) = ui.input(|i| (i.time, i.pixels_per_point));
        let pen = drain_pen();
        if !pen.is_empty() {
            self.filter.pen_seen(now);
        }
        for e in pen {
            self.levels.insert(e.pressure.to_bits());
            if matches!(e.phase, Phase::Down | Phase::Move) {
                let color = if e.eraser {
                    Color32::RED
                } else {
                    Color32::from_rgb(40, 40, 40)
                };
                self.dots
                    .push((Pos2::new(e.pos[0], e.pos[1]) / ppp, e.pressure, color));
            }
            self.last = Some(e);
        }

        let (rect, _) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let presses: Vec<Pos2> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        ..
                    } => Some(*pos),
                    _ => None,
                })
                .collect()
        });
        for pos in presses.into_iter().filter(|p| rect.contains(*p)) {
            if self.filter.accept_mouse_press(now) {
                self.mouse_accepted += 1;
                self.dots.push((pos, 1.0, Color32::BLUE));
            } else {
                self.mouse_dropped += 1;
            }
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_rgb(236, 232, 224));
        for (p, pressure, color) in &self.dots {
            painter.circle_filled(*p, 1.0 + pressure * 12.0, *color);
        }
        let pen = match &self.last {
            Some(e) => format!(
                "pressure {:.4}  eraser {}  tilt {:.1}, {:.1}  barrel {}  phase {:?}",
                e.pressure, e.eraser, e.tilt[0], e.tilt[1], e.barrel, e.phase
            ),
            None => "no pen event yet".into(),
        };
        let text = format!(
            "API: {}\n{pen}\ndistinct pressure levels seen: {}\nmouse presses on canvas: {} drawn (blue), {} dropped as pen duplicates\nright-click clears",
            self.api,
            self.levels.len(),
            self.mouse_accepted,
            self.mouse_dropped
        );
        painter.text(
            rect.min + egui::vec2(8.0, 8.0),
            egui::Align2::LEFT_TOP,
            text,
            FontId::monospace(13.0),
            Color32::BLACK,
        );
        if ui.input(|i| i.pointer.secondary_clicked()) {
            self.dots.clear();
            self.levels.clear();
        }
    }
}
