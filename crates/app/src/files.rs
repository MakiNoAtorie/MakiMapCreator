//! Project files and app commands: new, open, save, restore, close; app data paths.

use std::path::PathBuf;

use mmc_core::{MapId, MapKindModule, Project};
use mmc_store::Store;

use crate::i18n::{tr, tr_args};
use crate::snapshot::Snapshot;
use crate::workspace::{AppCommand, Mode, Tab, Tabs};
use crate::{App, Doc, errors};

/// Project file extension.
pub const EXTENSION: &str = "mmc";

/// The app's folder in local app data.
pub fn app_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("MakiMapCreator")
}

fn working_dir() -> PathBuf {
    app_dir().join("working")
}

/// Working copies a crash left behind (ADR-0011); a running instance holds its own locked.
pub fn leftover_working_copies() -> Vec<PathBuf> {
    std::fs::read_dir(working_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == EXTENSION) && !Store::in_use(p))
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

impl App {
    pub(crate) fn run(&mut self, action: AppCommand) {
        log::info!("file command {action:?}");
        match action {
            AppCommand::New => {
                let root = mmc_kind_region::RegionKind.default_settings();
                let kinds = self.kinds.clone();
                let r = Store::create(&working_copy())
                    .map_err(Into::into)
                    .and_then(|s| Project::create(Box::new(s), kinds, root));
                self.replace(r.map(|p| (p, None)), "error-new");
            }
            AppCommand::Open => {
                if let Some(file) = file_dialog().pick_file() {
                    let kinds = self.kinds.clone();
                    let r = Store::open(&file, &working_copy())
                        .map_err(Into::into)
                        .and_then(|s| Project::load(Box::new(s), kinds));
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
            AppCommand::ExportPng => {
                if let Some(d) = &self.doc {
                    self.export.open(d.settings);
                }
            }
            AppCommand::Fit => self.view = None,
            AppCommand::EditMode => self.mode = Mode::Edit,
            AppCommand::GenerateMode => self.mode = Mode::Generate.coerce(self.has_generators()),
            AppCommand::TogglePanels => self.panels_hidden = !self.panels_hidden,
        }
    }

    /// Restores (`true`) or discards the first leftover working copy.
    pub(crate) fn resolve_leftover(&mut self, restore: bool) {
        log::info!("leftover working copy: restore = {restore}");
        let leftover = self.leftovers.remove(0);
        if restore {
            let kinds = self.kinds.clone();
            let r = Store::restore(&leftover)
                .map_err(Into::into)
                .and_then(|s| Project::load(Box::new(s), kinds));
            self.replace(r.map(|p| (p, None)), "error-restore");
        } else if let Err(e) = Store::discard(&leftover) {
            let e = mmc_core::Error::from(e);
            self.message = Some(tr_args("error-restore", &[("error", &errors::text(&e))]));
        }
    }

    /// Closes the open project and shows `next`, or reports its error.
    fn replace(&mut self, next: mmc_core::Result<(Project, Option<PathBuf>)>, error: &str) {
        let (project, file) = match next {
            Ok(n) => n,
            Err(e) => {
                self.message = Some(tr_args(error, &[("error", &errors::text(&e))]));
                return;
            }
        };
        let Some(map) = project.maps().first().cloned() else {
            return;
        };
        self.close();
        // Generator runs default to the project seed, so neighbouring child maps match.
        self.seed = project.seed().unwrap_or(1);
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
        self.regen = None;
        self.mode = Mode::Edit;
        self.view = None;
        self.message = None;
        if let Some(c) = &mut self.canvas {
            c.clear();
        }
    }

    pub(crate) fn close(&mut self) {
        if let Some((j, _)) = self.job.take() {
            let _ = j.join();
        }
        self.pending.clear();
        self.pending_save = None;
        self.saving = false;
        self.editor.sync();
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
    pub(crate) fn switch_map(&mut self, id: MapId) {
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
        self.regen = None;
        self.dirty = true;
        self.mode = self.mode.coerce(self.has_generators());
        if let Some(c) = &mut self.canvas {
            c.clear();
        }
    }

    pub(crate) fn has_generators(&self) -> bool {
        !self.generators().is_empty()
    }

    /// The shown map's generators that can run on it; a root map has none that refine a parent.
    pub(crate) fn generators(&self) -> Vec<&'static mmc_core::GeneratorDecl> {
        let Some(d) = &self.doc else { return vec![] };
        let (Some(k), Some(m)) = (
            self.kinds.get(d.settings.kind),
            self.snap.maps.iter().find(|m| m.id == d.map),
        ) else {
            return vec![];
        };
        k.generators().iter().filter(|g| g.runs_on(m)).collect()
    }
}
