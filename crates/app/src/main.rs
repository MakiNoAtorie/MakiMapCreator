mod brush;
mod canvas;
mod i18n;
mod input;
mod paint;
mod panels;
#[cfg(windows)]
mod pen_win;
mod status;
mod theme;
mod view;
mod workspace;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use canvas::{Canvas, SharedProject};
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, Sense, TextStyle};
use i18n::{tr, tr_args};
use input::{DuplicatePressFilter, PenApi, PenEvent, Phase};
use mmc_core::{
    Command, Entity, EntityEdit, EntityId, FieldDef, Kinds, Layer, Map, MapId, MapKind,
    MapSettings, Project, RootPoint,
};
use mmc_store::Store;
use paint::Painter;
use panels::{Intent, Pick, Search};
use theme::Theme;
use view::MapView;
use workspace::{AppCommand, Mode, Tab, Tabs};

/// Pen API setting. No settings UI yet; `cargo run -p mmc-app -- ink` overrides it.
const PEN_API: PenApi = PenApi::WinTab;
/// Seconds after a pen event during which a mouse press counts as the pen's duplicate.
const DUPLICATE_PRESS_WINDOW: f64 = 1.0;
/// Project file extension.
const EXTENSION: &str = "mmc";
/// Zoom factor per scroll point.
const ZOOM_PER_POINT: f64 = 1.002;
/// Name hits shown in command search.
const SEARCH_NAMES: usize = 8;

const NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SAVE_AS: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
const FIT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Home);
const SEARCH: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F3);
const PANELS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Tab);

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

/// Project data the panels show. Reloaded when the project changed, so drawing never queries (ADR-0014).
#[derive(Default)]
struct Snapshot {
    maps: Vec<Map>,
    entities: Vec<Entity>,
    defs: HashMap<String, Vec<FieldDef>>,
    /// Outlines of the entities on the shown map.
    outlines: Vec<(EntityId, Vec<RootPoint>)>,
    rows: Vec<workspace::Row>,
    last_run: Option<String>,
}

impl Snapshot {
    fn load(p: &Project, map: MapId) -> mmc_core::Result<Snapshot> {
        let entities = p.entities()?;
        let relations = p.relations()?;
        let kinds = p.entity_kinds()?;
        let mut defs = HashMap::new();
        for k in &kinds {
            defs.insert(k.id.clone(), p.field_defs(&k.id)?);
        }
        let last_run = p.generations(map)?.last().map(|g| {
            tr_args(
                "generate-last",
                &[
                    ("generator", &g.generator),
                    ("seed", &g.seed.to_string()),
                    ("version", &g.app_version),
                ],
            )
        });
        Ok(Snapshot {
            maps: p.maps().to_vec(),
            rows: workspace::outliner_rows(&entities, &relations, &kinds),
            outlines: p.outlines(map)?,
            entities,
            defs,
            last_run,
        })
    }
}

/// Slow work on a worker that holds the project lock (ADR-0014).
enum Done {
    Generated(MapId),
    Child(MapId),
}

type Job = JoinHandle<mmc_core::Result<Done>>;

struct App {
    dark: bool,
    project: SharedProject,
    kinds: Arc<Kinds>,
    doc: Option<Doc>,
    snap: Snapshot,
    /// The snapshot is out of date.
    dirty: bool,
    tabs: Tabs,
    mode: Mode,
    selection: Option<EntityId>,
    hidden: HashSet<Layer>,
    field_edit: panels::CellEdit,
    page: Option<(EntityId, String)>,
    generator: usize,
    seed: u64,
    search: Search,
    search_names: Vec<(EntityId, String)>,
    panels_hidden: bool,
    job: Option<Job>,
    /// A child map to show once the snapshot has it.
    open_after: Option<MapId>,
    /// Intents that found the project busy.
    pending: Vec<Intent>,
    /// Working copies a crash left behind, to restore or discard.
    leftovers: Vec<PathBuf>,
    pending_save: Option<PathBuf>,
    saving: bool,
    /// `None` fits the map on the next frame.
    view: Option<MapView>,
    canvas: Option<Canvas>,
    message: Option<String>,
    hover: Option<RootPoint>,
    elevation: Option<f32>,
    pen_api: String,
    pen_filter: DuplicatePressFilter,
    painters: HashMap<MapId, Painter>,
    painter_seen: Option<u64>,
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
        let kinds = Kinds::new(vec![
            Box::new(mmc_kind_world::WorldKind),
            Box::new(mmc_kind_region::RegionKind),
        ]);
        Self {
            dark,
            project,
            kinds: Arc::new(kinds),
            doc: None,
            snap: Snapshot::default(),
            dirty: false,
            tabs: Tabs::default(),
            mode: Mode::Edit,
            selection: None,
            hidden: HashSet::new(),
            field_edit: None,
            page: None,
            generator: 0,
            seed: 1,
            search: Search::default(),
            search_names: vec![],
            panels_hidden: false,
            job: None,
            open_after: None,
            pending: vec![],
            leftovers: leftover_working_copies(),
            pending_save: None,
            saving: false,
            view: None,
            canvas,
            message: None,
            hover: None,
            elevation: None,
            pen_api: start_pen(cc, setting),
            pen_filter: DuplicatePressFilter::new(DUPLICATE_PRESS_WINDOW),
            painters: HashMap::new(),
            painter_seen: None,
            pen_events: Vec::new(),
            pen_down: false,
            mouse_down: false,
        }
    }

    fn run(&mut self, action: AppCommand) {
        match action {
            AppCommand::New => {
                let r = Store::create(&working_copy())
                    .map_err(Into::into)
                    .and_then(|s| {
                        Project::create(Box::new(s), MapSettings::default_for(MapKind::Region))
                    });
                self.replace(r.map(|p| (p, None)), "error-new");
            }
            AppCommand::Open => {
                if let Some(file) = file_dialog().pick_file() {
                    let r = Store::open(&file, &working_copy())
                        .map_err(Into::into)
                        .and_then(|s| Project::load(Box::new(s)));
                    self.replace(r.map(|p| (p, Some(file))), "error-open");
                }
            }
            AppCommand::Save => match self.doc.as_ref().map(|d| d.file.clone()) {
                Some(Some(file)) => self.pending_save = Some(file),
                Some(None) => self.run(AppCommand::SaveAs),
                None => {}
            },
            AppCommand::SaveAs => {
                if self.doc.is_some()
                    && let Some(file) = file_dialog().save_file()
                {
                    let file = file.with_extension(EXTENSION);
                    if let Some(d) = &mut self.doc {
                        d.file = Some(file.clone());
                    }
                    self.pending_save = Some(file);
                }
            }
            AppCommand::Fit => self.view = None,
            AppCommand::EditMode => self.mode = Mode::Edit,
            AppCommand::GenerateMode => self.mode = Mode::Generate.coerce(self.has_generators()),
            AppCommand::TogglePanels => self.panels_hidden = !self.panels_hidden,
        }
    }

    fn has_generators(&self) -> bool {
        self.doc
            .as_ref()
            .and_then(|d| self.kinds.get(d.settings.kind))
            .is_some_and(|k| !k.generators().is_empty())
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
        self.snap = Snapshot {
            maps: project.maps().to_vec(),
            ..Default::default()
        };
        *self.project.lock().unwrap() = Some(project);
        self.doc = Some(Doc {
            file,
            map: map.id,
            settings: map.settings,
        });
        self.tabs = Tabs::only(Tab::Map(map.id));
        self.dirty = true;
        self.selection = None;
        self.hidden.clear();
        self.field_edit = None;
        self.page = None;
        self.mode = Mode::Edit;
        self.view = None;
        self.message = None;
        if let Some(c) = &mut self.canvas {
            c.clear();
        }
    }

    fn close(&mut self) {
        if let Some(j) = self.job.take() {
            let _ = j.join();
        }
        self.pending.clear();
        self.pending_save = None;
        self.saving = false;
        if let Some(mut p) = self.project.lock().unwrap().take()
            && let Err(e) = p.wait_for_save().and_then(|()| p.close())
        {
            eprintln!("{e}");
        }
        self.doc = None;
        self.painters.clear();
        self.painter_seen = None;
    }

    /// Shows `id` in the canvas.
    fn switch_map(&mut self, id: MapId) {
        let Some(m) = self.snap.maps.iter().find(|m| m.id == id) else {
            return;
        };
        if let Some(d) = &mut self.doc {
            d.map = id;
            d.settings = m.settings;
        }
        self.view = None;
        self.selection = None;
        self.field_edit = None;
        self.dirty = true;
        self.mode = self.mode.coerce(self.has_generators());
        if let Some(c) = &mut self.canvas {
            c.clear();
        }
    }

    /// Counts a command applied outside the brush as one undo step of `map`.
    fn note_command(&mut self, map: MapId, now: f64, derived: bool) {
        let mpp = self
            .snap
            .maps
            .iter()
            .find(|m| m.id == map)
            .map_or(1.0, |m| m.settings.metres_per_pixel);
        let p = self
            .painters
            .entry(map)
            .or_insert_with(|| Painter::new(mpp));
        if let Some(c) = &mut self.canvas {
            p.external_command(c, now, derived);
        }
        self.dirty = true;
    }

    fn spawn(
        &mut self,
        work: impl FnOnce(&mut Project, &Kinds) -> mmc_core::Result<Done> + Send + 'static,
    ) {
        if self.job.is_some() {
            return;
        }
        let (project, kinds) = (self.project.clone(), self.kinds.clone());
        self.job = Some(std::thread::spawn(move || {
            match project.lock().unwrap().as_mut() {
                Some(p) => work(p, &kinds),
                None => Err(mmc_core::Error::InvalidChild("no project is open")),
            }
        }));
    }

    fn finish_job(&mut self, now: f64) {
        if !self.job.as_ref().is_some_and(|j| j.is_finished()) {
            return;
        }
        let Some(job) = self.job.take() else {
            return;
        };
        match job.join() {
            Ok(Ok(Done::Generated(m))) => self.note_command(m, now, true),
            Ok(Ok(Done::Child(c))) => {
                self.dirty = true;
                self.open_after = Some(c);
            }
            Ok(Err(e)) => {
                let id = if matches!(e, mmc_core::Error::InvalidChild(_)) {
                    "error-child"
                } else {
                    "error-generate"
                };
                self.message = Some(tr_args(id, &[("error", &e.to_string())]));
            }
            Err(_) => self.message = Some(tr_args("error-generate", &[("error", "panic")])),
        }
    }

    /// Writes the snapshot and save state while the project is free.
    fn sync(&mut self) {
        let Some(map) = self.doc.as_ref().map(|d| d.map) else {
            return;
        };
        let seen = self.painters.get(&map).map(|p| p.version);
        if seen != self.painter_seen {
            self.painter_seen = seen;
            self.dirty = true;
        }
        let Ok(mut guard) = self.project.try_lock() else {
            return;
        };
        let Some(p) = guard.as_mut() else {
            return;
        };
        if let Some(file) = self.pending_save.take() {
            p.save(&file);
            self.saving = true;
        }
        if self.saving
            && let Some(r) = p.poll_save()
        {
            self.saving = false;
            if let Err(e) = r {
                self.message = Some(tr_args("error-save", &[("error", &e.to_string())]));
            }
        }
        if self.dirty {
            self.dirty = false;
            match Snapshot::load(p, map) {
                Ok(s) => self.snap = s,
                Err(e) => self.message = Some(tr_args("error-read", &[("error", &e.to_string())])),
            }
            if self
                .selection
                .is_some_and(|s| !self.snap.entities.iter().any(|e| e.id == s))
            {
                self.selection = None;
            }
            let es = &self.snap.entities;
            self.tabs.retain(|t| match t {
                Tab::Page(id) => es.iter().any(|e| e.id == *id),
                Tab::Map(_) => true,
            });
        }
        if let Some(Tab::Page(id)) = self.tabs.active()
            && self.page.as_ref().map(|x| x.0) != Some(id)
        {
            match p.page(id) {
                Ok(text) => self.page = Some((id, text)),
                Err(e) => self.message = Some(tr_args("error-read", &[("error", &e.to_string())])),
            }
        }
    }

    fn apply(&mut self, intents: Vec<Intent>, now: f64) {
        let Some(map) = self.doc.as_ref().map(|d| d.map) else {
            return;
        };
        for intent in intents {
            match intent {
                Intent::Select(s) => {
                    self.selection = s;
                    self.field_edit = None;
                }
                Intent::SetField { .. } | Intent::Rename { .. } => {
                    let edit = match &intent {
                        Intent::SetField { id, key, value } => EntityEdit::SetField {
                            id: *id,
                            key: key.clone(),
                            value: value.clone(),
                        },
                        Intent::Rename { id, name } => EntityEdit::Rename {
                            id: *id,
                            name: name.clone(),
                        },
                        _ => continue,
                    };
                    let Ok(mut g) = self.project.try_lock() else {
                        self.pending.push(intent);
                        continue;
                    };
                    let r = g
                        .as_mut()
                        .map(|p| p.apply(map, Command::Entity(edit)))
                        .unwrap_or(Ok(()));
                    drop(g);
                    match r {
                        Ok(()) => self.note_command(map, now, false),
                        Err(e) => {
                            self.message = Some(tr_args("error-edit", &[("error", &e.to_string())]))
                        }
                    }
                }
                Intent::OpenMap(m) => self.tabs.open(Tab::Map(m)),
                Intent::OpenPage(e) => self.tabs.open(Tab::Page(e)),
                Intent::ActivateTab(i) => {
                    if i < self.tabs.items.len() {
                        self.tabs.active = i;
                    }
                }
                Intent::CloseTab(i) => self.tabs.close(i),
                Intent::CreateChild(entity) => {
                    self.spawn(move |p, kinds| {
                        p.create_child_map(kinds, map, entity, None)
                            .map(Done::Child)
                    });
                }
                Intent::Generate { generator, seed } => {
                    self.spawn(move |p, kinds| {
                        let kind = p
                            .maps()
                            .iter()
                            .find(|m| m.id == map)
                            .map(|m| m.settings.kind)
                            .ok_or(mmc_core::Error::UnknownMap(map))?;
                        let k = kinds
                            .get(kind)
                            .ok_or_else(|| mmc_core::Error::UnknownGenerator(generator.into()))?;
                        let cmd = k.generate(p, map, generator, seed)?;
                        p.apply(map, cmd)?;
                        Ok(Done::Generated(map))
                    });
                }
                Intent::ToggleLayer(l) => {
                    if !self.hidden.remove(&l) {
                        self.hidden.insert(l);
                    }
                }
                Intent::SetMode(m) => self.mode = m.coerce(self.has_generators()),
            }
        }
        if let Some(c) = self.open_after
            && self.snap.maps.iter().any(|m| m.id == c)
        {
            self.open_after = None;
            self.tabs.open(Tab::Map(c));
        }
        if let Some(Tab::Map(m)) = self.tabs.active()
            && m != map
        {
            self.switch_map(m);
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui, intents: &mut Vec<Intent>) -> Option<AppCommand> {
        let mut action = None;
        let item = |ui: &mut egui::Ui,
                    id: &str,
                    s: &KeyboardShortcut,
                    a: AppCommand,
                    act: &mut Option<AppCommand>| {
            let b = egui::Button::new(tr(id)).shortcut_text(ui.ctx().format_shortcut(s));
            if ui.add(b).clicked() {
                *act = Some(a);
            }
        };
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(tr("menu-file"), |ui| {
                item(ui, "menu-file-new", &NEW, AppCommand::New, &mut action);
                item(ui, "menu-file-open", &OPEN, AppCommand::Open, &mut action);
                item(ui, "menu-file-save", &SAVE, AppCommand::Save, &mut action);
                item(
                    ui,
                    "menu-file-save-as",
                    &SAVE_AS,
                    AppCommand::SaveAs,
                    &mut action,
                );
            });
            ui.menu_button(tr("menu-view"), |ui| {
                item(ui, "menu-view-fit", &FIT, AppCommand::Fit, &mut action);
                item(
                    ui,
                    "menu-view-panels",
                    &PANELS,
                    AppCommand::TogglePanels,
                    &mut action,
                );
                if ui.checkbox(&mut self.dark, tr("menu-view-dark")).changed() {
                    Theme::new(self.dark).apply(ui.ctx(), self.dark);
                }
            });
            if let Some(d) = &self.doc {
                ui.separator();
                if let Some(name) = d.file.as_ref().and_then(|f| f.file_stem()) {
                    ui.strong(name.to_string_lossy());
                    ui.weak("·");
                }
                panels::breadcrumb(ui, &self.snap.maps, d.map, intents);
                ui.weak(status::map_kind(d.settings.kind));
                let gens = self.has_generators();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    panels::mode_switch(ui, self.mode, gens, intents)
                });
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

    /// Edit mode paints with the brush on kinds that have a height tool.
    fn can_paint(&self) -> bool {
        self.mode == Mode::Edit
            && self.doc.as_ref().is_some_and(|d| {
                self.kinds
                    .get(d.settings.kind)
                    .is_some_and(|k| k.tools().iter().any(|t| t.layer == Layer::Height))
            })
    }

    fn side_panel(&mut self, ui: &mut egui::Ui, t: &Theme, intents: &mut Vec<Intent>) {
        let Some(d) = &self.doc else {
            return;
        };
        let kinds = self.kinds.clone();
        let kind = kinds.get(d.settings.kind);
        let children: Vec<_> = self
            .snap
            .maps
            .iter()
            .filter(|m| m.parent == Some(d.map))
            .map(|m| (m.id, workspace::map_label(&self.snap.maps, m.id)))
            .collect();
        panels::outliner(ui, &self.snap.rows, self.selection, &children, t, intents);
        ui.separator();
        let sel = self
            .selection
            .and_then(|s| self.snap.entities.iter().find(|e| e.id == s));
        let input = sel.and_then(|e| {
            Some(panels::PropsInput {
                entity: e,
                defs: self.snap.defs.get(&e.kind)?,
                entities: &self.snap.entities,
                can_child: kind.is_some_and(|k| k.child_kind().is_some())
                    && self.snap.outlines.iter().any(|o| o.0 == e.id),
            })
        });
        panels::properties(ui, input, &mut self.field_edit, t, intents);
        ui.separator();
        panels::layers(ui, kind, &self.hidden, t, intents);
    }

    fn map_canvas(
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
        // Click selects the place under the cursor; with the brush active it needs Ctrl.
        if response.clicked_by(egui::PointerButton::Primary)
            && (!can_paint || ui.input(|i| i.modifiers.command))
            && let Some(pos) = response.interact_pointer_pos()
        {
            intents.push(Intent::Select(workspace::pick(
                &self.snap.outlines,
                view.root_at(rect, pos),
            )));
        }
        let painter = self
            .painters
            .entry(map)
            .or_insert_with(|| Painter::new(s.metres_per_pixel));
        if can_paint {
            let mut sent = false;
            for e in std::mem::take(&mut self.pen_events) {
                let pos = egui::pos2(e.pos[0], e.pos[1]) / ppp;
                match e.phase {
                    Phase::Down => self.pen_down = rect.contains(pos),
                    Phase::Up if !self.pen_down => continue,
                    Phase::Move if !self.pen_down => continue,
                    _ => {}
                }
                painter.input(
                    &paint::Input {
                        pos,
                        pressure: e.pressure,
                        phase: e.phase,
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
        } else {
            self.pen_events.clear();
            self.pen_down = false;
            self.mouse_down = false;
            painter.hide_cursor();
        }
        self.hover = cursor.map(|p| view.root_at(rect, p));
        // Keep the last value while a worker holds the project (ADR-0014).
        if let Ok(guard) = self.project.try_lock()
            && let Some(p) = guard.as_ref()
        {
            self.elevation = self
                .hover
                .and_then(|r| p.layer_value(map, Layer::Height, r).ok().flatten());
        }
        let layers: Vec<Layer> = self
            .kinds
            .get(s.kind)
            .map(|k| {
                k.render_layers()
                    .iter()
                    .copied()
                    .filter(|l| !self.hidden.contains(l))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(c) = &mut self.canvas {
            if let Some(m) = painter.flush(&self.project, map, c, ui.ctx(), now) {
                self.message = Some(m);
            }
            c.show(ui, rect, map, layers, view);
            if let Some(e) = &c.error {
                self.message = Some(tr_args("error-render", &[("error", e)]));
            }
        }
        painter.draw(ui, rect, view, &Theme::new(self.dark));
        if let Some(sel) = self.selection
            && let Some((_, pts)) = self.snap.outlines.iter().find(|o| o.0 == sel)
        {
            let pts = pts.iter().map(|q| view.screen_at(rect, *q)).collect();
            let accent = Theme::new(self.dark).accent;
            ui.painter_at(rect).add(egui::Shape::closed_line(
                pts,
                egui::Stroke::new(2.0, accent),
            ));
        }
    }

    fn page_view(&self, ui: &mut egui::Ui) {
        let text = self.page.as_ref().map_or("", |p| p.1.as_str());
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(8.0);
            if text.is_empty() {
                ui.weak(tr("page-empty"));
            } else {
                ui.label(egui::RichText::new(text).family(egui::FontFamily::Name("serif".into())));
            }
        });
    }

    /// F3 window: commands that match, then place names from the name index.
    fn command_search(&mut self, ctx: &egui::Context) -> Option<AppCommand> {
        if !self.search.open {
            return None;
        }
        let labels: Vec<_> = workspace::COMMANDS
            .iter()
            .map(|(c, id)| (*c, tr(id)))
            .collect();
        let commands = workspace::rank_commands(&self.search.query, &labels);
        if !self.search.query.trim().is_empty()
            && let Ok(g) = self.project.try_lock()
            && let Some(p) = g.as_ref()
        {
            self.search_names = p
                .find_names(&self.search.query, SEARCH_NAMES)
                .into_iter()
                .map(|h| (h.id, h.name))
                .collect();
        } else if self.search.query.trim().is_empty() {
            self.search_names.clear();
        }
        match panels::command_search(ctx, &mut self.search, &commands, &self.search_names)? {
            Pick::Command(c) => Some(c),
            Pick::Entity(e) => {
                self.pending.push(Intent::Select(Some(e)));
                None
            }
        }
    }
}

fn working_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("MakiMapCreator").join("working")
}

/// Working copies from earlier runs; a crash left them (ADR-0011).
fn leftover_working_copies() -> Vec<PathBuf> {
    std::fs::read_dir(working_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == EXTENSION))
        .collect()
}

/// A new, unused working-copy path in local app data.
fn working_copy() -> PathBuf {
    let dir = working_dir();
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
        self.finish_job(now);
        self.sync();
        if self.saving || self.pending_save.is_some() || self.job.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
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
            if let Some(p) = self
                .doc
                .as_ref()
                .and_then(|d| self.painters.get_mut(&d.map))
            {
                edits.into_iter().for_each(|e| p.edit(e));
            }
        }
        let typing = ui.ctx().egui_wants_keyboard_input();
        let mut action = ui.input_mut(|i| {
            [
                (SAVE_AS, AppCommand::SaveAs),
                (NEW, AppCommand::New),
                (OPEN, AppCommand::Open),
                (SAVE, AppCommand::Save),
                (FIT, AppCommand::Fit),
                (PANELS, AppCommand::TogglePanels),
            ]
            .into_iter()
            .filter(|(_, a)| !typing || !matches!(a, AppCommand::Fit | AppCommand::TogglePanels))
            .find(|(s, _)| i.consume_shortcut(s))
            .map(|(_, a)| a)
        });
        if ui.input_mut(|i| i.consume_shortcut(&SEARCH)) && self.doc.is_some() {
            self.search = Search {
                open: true,
                query: String::new(),
                focus: true,
            };
        }
        let mut intents = std::mem::take(&mut self.pending);
        let bar = |m: i8, n: i8| {
            egui::Frame::new()
                .fill(t.panel)
                .inner_margin(egui::Margin::symmetric(m, n))
        };
        let show = self.doc.is_some() && !self.panels_hidden;
        if !self.panels_hidden {
            egui::Panel::top("menu").frame(bar(6, 2)).show(ui, |ui| {
                action = self.menu_bar(ui, &mut intents).or(action);
            });
        }
        if show {
            egui::Panel::top("tabs").frame(bar(6, 2)).show(ui, |ui| {
                panels::tab_bar(
                    ui,
                    &self.tabs,
                    &self.snap.maps,
                    &self.snap.entities,
                    &mut intents,
                )
            });
            let can_paint = self.can_paint();
            let map = self.doc.as_ref().map(|d| d.map);
            if can_paint && let Some(p) = map.and_then(|m| self.painters.get_mut(&m)) {
                egui::Panel::top("options")
                    .frame(bar(8, 3))
                    .show(ui, |ui| p.options_bar(ui));
                egui::Panel::left("tools")
                    .frame(bar(4, 4))
                    .show(ui, |ui| p.tool_strip(ui, &t));
            } else if self.mode == Mode::Generate {
                let kinds = self.kinds.clone();
                if let Some(kind) = self.doc.as_ref().and_then(|d| kinds.get(d.settings.kind)) {
                    let (busy, last) = (self.job.is_some(), self.snap.last_run.clone());
                    egui::Panel::left("generate")
                        .frame(bar(8, 6))
                        .default_size(200.0)
                        .show(ui, |ui| {
                            panels::generate_panel(
                                ui,
                                panels::GenInput {
                                    kind,
                                    generator: &mut self.generator,
                                    seed: &mut self.seed,
                                    busy,
                                    last,
                                },
                                &t,
                                &mut intents,
                            )
                        });
                }
            } else {
                egui::Panel::left("tools").frame(bar(8, 6)).show(ui, |ui| {
                    ui.weak(tr("edit-no-tools"));
                });
            }
            egui::Panel::right("side")
                .frame(bar(8, 6))
                .default_size(270.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        self.side_panel(ui, &t, &mut intents);
                    });
                });
        }
        if !self.panels_hidden {
            egui::Panel::bottom("status")
                .frame(bar(8, 3))
                .show(ui, |ui| self.status_bar(ui));
        }
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
                                action = Some(AppCommand::New);
                            }
                        });
                    });
                } else if matches!(self.tabs.active(), Some(Tab::Page(_))) {
                    self.page_view(ui);
                } else {
                    self.map_canvas(ui, pen_hover, &mut intents);
                }
            });
        action = self.command_search(ui.ctx()).or(action);
        if let Some(leftover) = self.leftovers.first().cloned()
            && let Some(restore) = panels::restore_prompt(ui.ctx())
        {
            self.leftovers.remove(0);
            if restore {
                let r = Store::restore(&leftover)
                    .map_err(Into::into)
                    .and_then(|s| Project::load(Box::new(s)));
                self.replace(r.map(|p| (p, None)), "error-restore");
            } else if let Err(e) = Store::discard(&leftover) {
                self.message = Some(tr_args("error-restore", &[("error", &e.to_string())]));
            }
        }
        if let Some(a) = action {
            self.run(a);
        }
        self.apply(intents, now);
    }

    fn on_exit(&mut self) {
        self.close();
    }
}
