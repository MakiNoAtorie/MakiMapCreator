mod brush;
mod canvas;
mod i18n;
mod input;
mod paint;
#[cfg(windows)]
mod pen_win;
mod status;
mod theme;
mod view;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use canvas::{Canvas, SharedProject};
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, Sense, TextStyle};
use i18n::{tr, tr_args};
use input::{DuplicatePressFilter, PenApi, PenEvent, Phase};
use mmc_core::{Layer, MapId, MapKind, MapSettings, Project, RootPoint};
use paint::Painter;
use theme::Theme;
use view::MapView;

/// Pen API setting. No settings UI yet; `cargo run -p mmc-app -- ink` overrides it.
const PEN_API: PenApi = PenApi::WinTab;
/// Seconds after a pen event during which a mouse press counts as the pen's duplicate.
const DUPLICATE_PRESS_WINDOW: f64 = 1.0;
/// Project file extension.
const EXTENSION: &str = "mmc";
/// Zoom factor per scroll point.
const ZOOM_PER_POINT: f64 = 1.002;

const NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SAVE_AS: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
const FIT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Home);

fn main() -> eframe::Result {
    let setting = match std::env::args().nth(1).as_deref() {
        Some("ink") => PenApi::Ink,
        _ => PEN_API,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1440.0, 880.0]),
        ..Default::default()
    };
    eframe::run_native(
        &tr("app-title"),
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, setting)))),
    )
}

/// The open project's file and shown map. The project itself is in `App::project`.
struct Doc {
    file: Option<PathBuf>,
    map: MapId,
    settings: MapSettings,
}

#[derive(Clone, Copy)]
enum Action {
    New,
    Open,
    Save,
    SaveAs,
    Fit,
}

struct App {
    dark: bool,
    project: SharedProject,
    doc: Option<Doc>,
    /// `None` fits the map on the next frame.
    view: Option<MapView>,
    canvas: Option<Canvas>,
    saving: Option<JoinHandle<mmc_core::Result<()>>>,
    message: Option<String>,
    hover: Option<RootPoint>,
    elevation: Option<f32>,
    pen_api: String,
    pen_filter: DuplicatePressFilter,
    painter: Option<Painter>,
    /// Pen events of this frame, taken by the canvas.
    pen_events: Vec<PenEvent>,
    /// A pen stroke is running (it began on the canvas).
    pen_down: bool,
    mouse_down: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext, setting: PenApi) -> Self {
        let dark = cc.egui_ctx.system_theme() != Some(egui::Theme::Light);
        theme::install_fonts(&cc.egui_ctx);
        Theme::new(dark).apply(&cc.egui_ctx, dark);
        let project: SharedProject = Arc::new(Mutex::new(None));
        let canvas = cc
            .wgpu_render_state
            .as_ref()
            .map(|rs| Canvas::new(rs, project.clone(), cc.egui_ctx.clone()));
        Self {
            dark,
            project,
            doc: None,
            view: None,
            canvas,
            saving: None,
            message: None,
            hover: None,
            elevation: None,
            pen_api: start_pen(cc, setting),
            pen_filter: DuplicatePressFilter::new(DUPLICATE_PRESS_WINDOW),
            painter: None,
            pen_events: Vec::new(),
            pen_down: false,
            mouse_down: false,
        }
    }

    fn run(&mut self, action: Action) {
        match action {
            Action::New => {
                let r = Project::create(&working_copy(), MapSettings::default_for(MapKind::Region));
                self.replace(r.map(|p| (p, None)), "error-new");
            }
            Action::Open => {
                if let Some(file) = file_dialog().pick_file() {
                    let r = Project::open(&file, &working_copy());
                    self.replace(r.map(|p| (p, Some(file))), "error-open");
                }
            }
            Action::Save => match self.doc.as_ref().map(|d| d.file.clone()) {
                Some(Some(file)) => self.save(file),
                Some(None) => self.run(Action::SaveAs),
                None => {}
            },
            Action::SaveAs => {
                if self.doc.is_some()
                    && let Some(file) = file_dialog().save_file()
                {
                    let file = file.with_extension(EXTENSION);
                    if let Some(d) = &mut self.doc {
                        d.file = Some(file.clone());
                    }
                    self.save(file);
                }
            }
            Action::Fit => self.view = None,
        }
    }

    /// Closes the open project and shows `next`, or reports its error.
    fn replace(&mut self, next: mmc_core::Result<(Project, Option<PathBuf>)>, error: &str) {
        let (project, file) = match next {
            Ok(n) => n,
            Err(e) => {
                self.message = Some(tr_args(error, &[("error", &e.to_string())]));
                return;
            }
        };
        let Some(map) = project.maps().first().cloned() else {
            return;
        };
        self.close();
        self.painter = Some(Painter::new(map.settings.metres_per_pixel));
        *self.project.lock().unwrap() = Some(project);
        self.doc = Some(Doc {
            file,
            map: map.id,
            settings: map.settings,
        });
        self.view = None;
        self.message = None;
        if let Some(c) = &mut self.canvas {
            c.clear();
        }
    }

    fn close(&mut self) {
        if let Some(h) = self.saving.take() {
            let _ = h.join();
        }
        if let Some(p) = self.project.lock().unwrap().take()
            && let Err(e) = p.close()
        {
            eprintln!("{e}");
        }
        self.doc = None;
        self.painter = None;
    }

    /// Saves on a worker so the UI keeps running (ADR-0014).
    fn save(&mut self, file: PathBuf) {
        if self.saving.is_some() {
            return;
        }
        let project = self.project.clone();
        self.saving = Some(std::thread::spawn(move || {
            match project.lock().unwrap().as_ref() {
                Some(p) => p.save(&file),
                None => Ok(()),
            }
        }));
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        let item = |ui: &mut egui::Ui,
                    id: &str,
                    s: &KeyboardShortcut,
                    a: Action,
                    act: &mut Option<Action>| {
            let b = egui::Button::new(tr(id)).shortcut_text(ui.ctx().format_shortcut(s));
            if ui.add(b).clicked() {
                *act = Some(a);
            }
        };
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(tr("menu-file"), |ui| {
                item(ui, "menu-file-new", &NEW, Action::New, &mut action);
                item(ui, "menu-file-open", &OPEN, Action::Open, &mut action);
                item(ui, "menu-file-save", &SAVE, Action::Save, &mut action);
                item(
                    ui,
                    "menu-file-save-as",
                    &SAVE_AS,
                    Action::SaveAs,
                    &mut action,
                );
            });
            ui.menu_button(tr("menu-view"), |ui| {
                item(ui, "menu-view-fit", &FIT, Action::Fit, &mut action);
                if ui.checkbox(&mut self.dark, tr("menu-view-dark")).changed() {
                    Theme::new(self.dark).apply(ui.ctx(), self.dark);
                }
            });
            if let Some(d) = &self.doc {
                ui.separator();
                if let Some(name) = d.file.as_ref().and_then(|f| f.file_stem()) {
                    ui.strong(name.to_string_lossy());
                }
                ui.weak(status::map_kind(d.settings.kind));
            }
        });
        action
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.style_mut().override_text_style = Some(TextStyle::Small);
            if let Some(d) = &self.doc {
                ui.monospace(status::cursor(self.hover));
                ui.separator();
                ui.monospace(status::elevation(self.elevation));
                ui.separator();
                // Hook: entity under the cursor comes with anchors (#18).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(tr_args("status-pen", &[("api", &self.pen_api)]));
                    ui.separator();
                    ui.weak(status::map(&d.settings));
                });
            }
        });
    }

    fn map_canvas(&mut self, ui: &mut egui::Ui, pen_hover: Option<egui::Pos2>) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let Some(doc) = &self.doc else {
            return;
        };
        let s = doc.settings;
        let view = self.view.get_or_insert_with(|| {
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
        let (now, ppp) = ui.input(|i| (i.time, i.pixels_per_point));
        let cursor = pen_hover
            .or(response.hover_pos())
            .filter(|p| rect.contains(*p));
        if let Some(painter) = &mut self.painter {
            let mut sent = false;
            for e in std::mem::take(&mut self.pen_events) {
                let pos = egui::pos2(e.pos[0], e.pos[1]) / ppp;
                match e.phase {
                    Phase::Down => self.pen_down = rect.contains(pos),
                    Phase::Up if !self.pen_down => continue,
                    Phase::Move if !self.pen_down => continue,
                    _ => {}
                }
                let phase = if e.phase == Phase::Up {
                    Phase::Up
                } else {
                    e.phase
                };
                painter.input(
                    &paint::Input {
                        pos,
                        pressure: e.pressure,
                        phase,
                    },
                    view,
                    rect,
                );
                sent = true;
                if e.phase == Phase::Up {
                    self.pen_down = false;
                }
            }
            if !space {
                let pos = response.interact_pointer_pos().or(cursor);
                if response.drag_started_by(egui::PointerButton::Primary)
                    && self.pen_filter.accept_mouse_press(now)
                {
                    self.mouse_down = true;
                    if let Some(pos) = pos {
                        painter.input(
                            &paint::Input {
                                pos,
                                pressure: 1.0,
                                phase: Phase::Down,
                            },
                            view,
                            rect,
                        );
                        sent = true;
                    }
                } else if self.mouse_down
                    && let Some(pos) = pos
                {
                    let stopped = response.drag_stopped();
                    let phase = if stopped { Phase::Up } else { Phase::Move };
                    painter.input(
                        &paint::Input {
                            pos,
                            pressure: 1.0,
                            phase,
                        },
                        view,
                        rect,
                    );
                    sent = true;
                    self.mouse_down = !stopped;
                }
            }
            if !sent {
                match cursor {
                    Some(pos) => painter.input(
                        &paint::Input {
                            pos,
                            pressure: 0.0,
                            phase: Phase::Hover,
                        },
                        view,
                        rect,
                    ),
                    None => painter.hide_cursor(),
                }
            }
        }
        self.hover = cursor.map(|p| view.root_at(rect, p));
        // Keep the last value while a worker holds the project (ADR-0014).
        if let Ok(guard) = self.project.try_lock()
            && let Some(p) = guard.as_ref()
        {
            self.elevation = self
                .hover
                .and_then(|r| p.layer_value(doc.map, Layer::Height, r).ok().flatten());
        }
        if let (Some(p), Some(c)) = (&mut self.painter, &mut self.canvas)
            && let Some(m) = p.flush(&self.project, doc.map, c, ui.ctx(), now)
        {
            self.message = Some(m);
        }
        if let Some(c) = &mut self.canvas {
            c.show(ui, rect, doc.map, view);
            if let Some(e) = &c.error {
                self.message = Some(tr_args("error-render", &[("error", e)]));
            }
        }
        if let Some(p) = &self.painter {
            p.draw(ui, rect, view, &Theme::new(self.dark));
        }
    }
}

/// A new, unused working-copy path in local app data.
fn working_copy() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("MakiMapCreator").join("working");
    let _ = std::fs::create_dir_all(&dir);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    dir.join(format!("{}-{nanos}.{EXTENSION}", std::process::id()))
}

fn file_dialog() -> rfd::FileDialog {
    rfd::FileDialog::new().add_filter(tr("file-filter"), &[EXTENSION])
}

#[cfg(windows)]
fn start_pen(cc: &eframe::CreationContext, setting: PenApi) -> String {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(RawWindowHandle::Win32(h)) = cc.window_handle().map(|h| h.as_raw()) else {
        return String::new();
    };
    let s = pen_win::install(h.hwnd.get(), setting, cc.egui_ctx.clone());
    format!("{:?} · {}", s.api, s.wintab)
}

#[cfg(not(windows))]
fn start_pen(_: &eframe::CreationContext, _: PenApi) -> String {
    String::new()
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t = Theme::new(self.dark);
        let (now, ppp) = ui.input(|i| (i.time, i.pixels_per_point));
        let mut pen_hover = None;
        for e in drain_pen() {
            self.pen_events.push(e);
            self.pen_filter.pen_seen(now);
            pen_hover = (e.phase != Phase::Up).then(|| egui::pos2(e.pos[0], e.pos[1]) / ppp);
        }
        if let Some(h) = &self.saving
            && h.is_finished()
            && let Some(Err(e)) = self.saving.take().and_then(|h| h.join().ok())
        {
            self.message = Some(tr_args("error-save", &[("error", &e.to_string())]));
        }

        if !ui.ctx().egui_wants_keyboard_input() {
            let edits: Vec<_> = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Key {
                            key,
                            pressed: true,
                            modifiers,
                            ..
                        } => brush::route(*key, *modifiers),
                        _ => None,
                    })
                    .collect()
            });
            if let Some(p) = &mut self.painter {
                edits.into_iter().for_each(|e| p.edit(e));
            }
        }
        let mut action = ui.input_mut(|i| {
            [
                (SAVE_AS, Action::SaveAs),
                (NEW, Action::New),
                (OPEN, Action::Open),
                (SAVE, Action::Save),
                (FIT, Action::Fit),
            ]
            .into_iter()
            .find(|(s, _)| i.consume_shortcut(s))
            .map(|(_, a)| a)
        });
        let bar = |m: i8, n: i8| {
            egui::Frame::new()
                .fill(t.panel)
                .inner_margin(egui::Margin::symmetric(m, n))
        };
        egui::Panel::top("menu").frame(bar(6, 2)).show(ui, |ui| {
            action = self.menu_bar(ui).or(action);
        });
        if self.doc.is_some()
            && let Some(p) = &mut self.painter
        {
            egui::Panel::top("options")
                .frame(bar(8, 3))
                .show(ui, |ui| p.options_bar(ui));
            egui::Panel::left("tools")
                .frame(bar(4, 4))
                .show(ui, |ui| p.tool_strip(ui, &t));
        }
        egui::Panel::bottom("status")
            .frame(bar(8, 3))
            .show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(t.canvas))
            .show(ui, |ui| {
                if let Some(m) = &self.message {
                    ui.colored_label(ui.visuals().error_fg_color, m);
                }
                if self.doc.is_none() {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(tr("empty-text"));
                            if ui.button(tr("menu-file-new")).clicked() {
                                action = Some(Action::New);
                            }
                        });
                    });
                }
                self.map_canvas(ui, pen_hover);
            });
        if let Some(a) = action {
            self.run(a);
        }
    }

    fn on_exit(&mut self) {
        self.close();
    }
}
