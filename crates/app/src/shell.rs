//! The workspace frame: menu bar, tool panels, side panel, status bar, page view, dialogs.

use eframe::egui::{self, KeyboardShortcut, TextStyle};

use crate::i18n::{tr, tr_args};
use crate::panels::{self, Intent, Pick};
use crate::theme::Theme;
use crate::workspace::{self, AppCommand, Mode, Tab};
use crate::{App, EXPORT, FIT, NEW, OPEN, PANELS, SAVE, SAVE_AS, files, pen, status};

/// Name hits shown in command search.
const SEARCH_NAMES: usize = 8;

impl App {
    /// Draws all panels and the central area; returns a menu command.
    pub(crate) fn layout(
        &mut self,
        ui: &mut egui::Ui,
        t: &Theme,
        pen_hover: Option<egui::Pos2>,
        intents: &mut Vec<Intent>,
    ) -> Option<AppCommand> {
        let mut action = None;
        let bar = |m: i8, n: i8| {
            egui::Frame::new()
                .fill(t.panel)
                .inner_margin(egui::Margin::symmetric(m, n))
        };
        let show = self.doc.is_some() && !self.panels_hidden;
        if !self.panels_hidden {
            egui::Panel::top("menu").frame(bar(6, 2)).show(ui, |ui| {
                action = self.menu_bar(ui, intents);
            });
        }
        if show {
            egui::Panel::top("tabs").frame(bar(6, 2)).show(ui, |ui| {
                panels::tab_bar(
                    ui,
                    &self.tabs,
                    &self.snap.maps,
                    &self.snap.entities,
                    intents,
                )
            });
            let map = self.doc.as_ref().map(|d| d.map);
            if self.can_paint()
                && let Some(map) = map
            {
                let p = self.painter(map);
                egui::Panel::top("options")
                    .frame(bar(8, 3))
                    .show(ui, |ui| p.options_bar(ui));
                egui::Panel::left("tools")
                    .frame(bar(4, 4))
                    .show(ui, |ui| p.tool_strip(ui, t));
            } else if self.mode == Mode::Generate {
                self.generate_panel(ui, t, bar(8, 6), intents);
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
                        self.side_panel(ui, t, intents);
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
                    self.map_canvas(ui, pen_hover, intents);
                }
            });
        action
    }

    /// Edit mode paints with the kind's tools.
    pub(crate) fn can_paint(&self) -> bool {
        self.mode == Mode::Edit
            && self.doc.as_ref().is_some_and(|d| {
                self.kinds
                    .get(d.settings.kind)
                    .is_some_and(|k| !k.tools().is_empty())
            })
    }

    fn generate_panel(
        &mut self,
        ui: &mut egui::Ui,
        t: &Theme,
        frame: egui::Frame,
        intents: &mut Vec<Intent>,
    ) {
        let gens = self.generators();
        let (busy, last) = (
            self.job.is_some() || self.regen.is_some(),
            self.snap.last_run.clone(),
        );
        let selection = self
            .selection
            .and_then(|s| self.snap.outlines.iter().find(|o| o.0 == s))
            .map(|o| o.1.clone());
        egui::Panel::left("generate")
            .frame(frame)
            .default_size(200.0)
            .show(ui, |ui| {
                panels::generate_panel(
                    ui,
                    panels::GenInput {
                        gens,
                        generator: &mut self.generator,
                        seed: &mut self.seed,
                        params: &mut self.params,
                        busy,
                        last,
                        selection,
                    },
                    t,
                    intents,
                )
            });
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
                ui.separator();
                item(
                    ui,
                    "menu-file-export",
                    &EXPORT,
                    AppCommand::ExportPng,
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
                ui.separator();
                if let Some(api) = pen::menu(ui, self.pen_setting) {
                    self.pen_setting = api;
                    self.message = Some(match pen::save_setting(&files::app_dir(), api) {
                        Ok(()) => tr("pen-restart"),
                        Err(e) => tr_args("error-settings", &[("error", &e.to_string())]),
                    });
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
                ui.label(status::entity(self.hover_entity.as_deref()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(&self.pen_status);
                    ui.separator();
                    ui.weak(status::map(&d.settings));
                });
            }
        });
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
                can_child: kind.is_some_and(|k| k.child().is_some())
                    && self.snap.outlines.iter().any(|o| o.0 == e.id),
            })
        });
        panels::properties(ui, input, &mut self.field_edit, t, intents);
        ui.separator();
        panels::layers(ui, kind, &self.hidden, t, intents);
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
    pub(crate) fn command_search(&mut self, ctx: &egui::Context) -> Option<AppCommand> {
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

    /// Modal windows: crash restore, regeneration preview, PNG export.
    pub(crate) fn dialogs(&mut self, ctx: &egui::Context, intents: &mut Vec<Intent>) {
        if !self.leftovers.is_empty()
            && let Some(restore) = panels::restore_prompt(ctx)
        {
            self.resolve_leftover(restore);
        }
        if let Some(plan) = &self.regen
            && let Some(apply) = panels::regen_preview(ctx, plan)
        {
            intents.push(Intent::ApplyRegeneration(apply));
        }
        self.export_dialog(ctx);
    }
}
