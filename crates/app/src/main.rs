mod brush;
mod canvas;
mod edit;
mod errors;
mod export;
mod files;
mod i18n;
mod input;
mod intents;
mod jobs;
mod logfile;
mod map_canvas;
mod paint;
mod panels;
mod pen;
#[cfg(windows)]
mod pen_win;
#[cfg(test)]
mod perf;
mod shell;
mod snapshot;
mod status;
mod theme;
mod view;
mod workspace;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use canvas::{Canvas, SharedProject};
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use i18n::tr;
use input::{DuplicatePressFilter, PenApi, PenEvent, Phase};
use mmc_core::{EntityId, Kinds, Layer, MapId, MapSettings, RegenPlan, RootPoint};
use paint::Painter;
use panels::{Intent, Search};
use snapshot::Snapshot;
use theme::Theme;
use view::MapView;
use workspace::{AppCommand, Mode, Tabs};

/// Seconds after a pen event during which a mouse press counts as the pen's duplicate.
const DUPLICATE_PRESS_WINDOW: f64 = 1.0;
/// A frame, or a gap between frames while drawing, longer than this is logged.
const SLOW_FRAME: std::time::Duration = std::time::Duration::from_millis(50);

const NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SAVE_AS: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
const EXPORT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::E);
const FIT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Home);
const SEARCH: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F3);
const PANELS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Tab);

fn main() -> eframe::Result {
    logfile::init(&files::app_dir());
    let setting = pen::cli_override().unwrap_or_else(|| pen::load_setting(&files::app_dir()));
    log::info!("pen setting {setting:?}");
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    let fallback = setup.instance_descriptor.backends;
    setup.instance_descriptor.backends = mmc_render::preferred_backends(fallback);
    log::info!("wgpu backends {:?}", setup.instance_descriptor.backends);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1440.0, 880.0]),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(setup),
            ..Default::default()
        },
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

struct App {
    dark: bool,
    project: SharedProject,
    kinds: Arc<Kinds>,
    render_state: Option<eframe::egui_wgpu::RenderState>,
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
    params: Vec<(String, f64)>,
    search: Search,
    search_names: Vec<(EntityId, String)>,
    panels_hidden: bool,
    job: Option<(jobs::Job, &'static str)>,
    /// A previewed masked regeneration waiting for Apply or Cancel.
    regen: Option<Box<RegenPlan>>,
    export: export::Export,
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
    /// The last message written to the log.
    logged_message: Option<String>,
    hover: Option<RootPoint>,
    elevation: Option<f32>,
    /// Name of the entity under the cursor.
    hover_entity: Option<String>,
    pen_setting: PenApi,
    pen_status: String,
    pen_filter: DuplicatePressFilter,
    painters: HashMap<MapId, Painter>,
    editor: edit::Editor,
    painter_seen: Option<u64>,
    /// Pen events of this frame, taken by the canvas.
    pen_events: Vec<PenEvent>,
    /// A pen stroke is running (it began on the canvas).
    pen_down: bool,
    mouse_down: bool,
    /// Start of the last frame.
    frame_start: std::time::Instant,
}

impl App {
    fn new(cc: &eframe::CreationContext, setting: PenApi) -> Self {
        let dark = cc.egui_ctx.system_theme() != Some(egui::Theme::Light);
        theme::install_fonts(&cc.egui_ctx);
        Theme::new(dark).apply(&cc.egui_ctx, dark);
        let project: SharedProject = Arc::new(Mutex::new(None));
        let kinds = Arc::new(Kinds::new(vec![
            Box::new(mmc_kind_world::WorldKind),
            Box::new(mmc_kind_region::RegionKind),
        ]));
        let editor = edit::Editor::new(project.clone(), cc.egui_ctx.clone());
        let canvas = cc
            .wgpu_render_state
            .as_ref()
            .map(|rs| Canvas::new(rs, project.clone(), kinds.clone(), cc.egui_ctx.clone()));
        Self {
            dark,
            project,
            kinds,
            render_state: cc.wgpu_render_state.clone(),
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
            params: vec![],
            search: Search::default(),
            search_names: vec![],
            panels_hidden: false,
            job: None,
            regen: None,
            export: export::Export::default(),
            open_after: None,
            pending: vec![],
            leftovers: files::leftover_working_copies(),
            pending_save: None,
            saving: false,
            view: None,
            canvas,
            message: None,
            logged_message: None,
            hover: None,
            elevation: None,
            hover_entity: None,
            pen_setting: setting,
            pen_status: pen::start(cc, setting),
            pen_filter: DuplicatePressFilter::new(DUPLICATE_PRESS_WINDOW),
            painters: HashMap::new(),
            editor,
            painter_seen: None,
            pen_events: Vec::new(),
            pen_down: false,
            mouse_down: false,
            frame_start: std::time::Instant::now(),
        }
    }

    /// The painter of `map`, made with its kind's tools on first use.
    fn painter(&mut self, map: MapId) -> &mut Painter {
        let (mpp, kind) = self
            .snap
            .maps
            .iter()
            .find(|m| m.id == map)
            .map_or((1.0, None), |m| {
                (m.settings.metres_per_pixel, Some(m.settings.kind))
            });
        let tools = kind
            .and_then(|k| self.kinds.get(k))
            .map_or(&[][..], |k| k.tools());
        self.painters
            .entry(map)
            .or_insert_with(|| Painter::new(map, mpp, tools))
    }

    /// Notes a command applied outside the painter so the canvas and panels catch up.
    fn note_command(&mut self, map: MapId, derived: bool) {
        if derived {
            self.editor.send(edit::Op::Watch);
        }
        let mut canvas = self.canvas.take();
        if let Some(c) = &mut canvas {
            self.painter(map).changed(c);
        }
        self.canvas = canvas;
        self.dirty = true;
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let start = std::time::Instant::now();
        let gap = start - std::mem::replace(&mut self.frame_start, start);
        if (self.pen_down || self.mouse_down) && gap > SLOW_FRAME {
            log::warn!(
                "{:.0} ms between frames while drawing",
                gap.as_secs_f64() * 1000.0
            );
        }
        if self.message != self.logged_message {
            if let Some(m) = &self.message {
                log::warn!("message: {m}");
            }
            self.logged_message = self.message.clone();
        }
        let t = Theme::new(self.dark);
        let (now, ppp) = ui.input(|i| (i.time, i.pixels_per_point));
        let mut pen_hover = None;
        for e in pen::drain() {
            self.pen_events.push(e);
            self.pen_filter.pen_seen(now);
            pen_hover = (e.phase != Phase::Up).then(|| egui::pos2(e.pos[0], e.pos[1]) / ppp);
        }
        self.finish_job();
        self.finish_export();
        self.sync();
        if self.saving || self.pending_save.is_some() || self.job.is_some() || self.export.busy() {
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
            if let Some(map) = self.doc.as_ref().map(|d| d.map) {
                let p = self.painter(map);
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
                (EXPORT, AppCommand::ExportPng),
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
        action = self.layout(ui, &t, pen_hover, &mut intents).or(action);
        action = self.command_search(ui.ctx()).or(action);
        self.dialogs(ui.ctx(), &mut intents);
        if let Some(a) = action {
            self.run(a);
        }
        self.apply(intents);
        if start.elapsed() > SLOW_FRAME {
            log::warn!(
                "frame took {:.0} ms",
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }

    fn on_exit(&mut self) {
        self.close();
    }
}
