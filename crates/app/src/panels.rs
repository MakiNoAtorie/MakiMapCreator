//! Workspace panels: drawing only. Each panel reads state and pushes `Intent`s; `App` applies them.

use std::collections::HashSet;

use eframe::egui::{self, RichText};
use mmc_core::{
    Entity, EntityId, FieldDef, FieldType, GenRun, GeneratorDecl, Layer, Map, MapId, MapKindModule,
    RegenPlan, RootPoint,
};
use serde_json::Value;

use crate::i18n::{tr, tr_args};
use crate::theme::Theme;
use crate::workspace::{self, AppCommand, Mode, Row, Tab, Tabs};

/// A change the user asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    Select(Option<EntityId>),
    SetField {
        id: EntityId,
        key: String,
        value: Value,
    },
    Rename {
        id: EntityId,
        name: String,
    },
    OpenMap(MapId),
    OpenPage(EntityId),
    ActivateTab(usize),
    CloseTab(usize),
    CreateChild(EntityId),
    Generate(GenRun),
    /// Previews a masked regeneration inside the selection outline (root space).
    RegenerateSelection(GenRun, Vec<RootPoint>),
    /// Applies (`true`) or drops the previewed regeneration.
    ApplyRegeneration(bool),
    ToggleLayer(Layer),
    SetMode(Mode),
}

/// The property cell being typed in: (entity, key, text).
pub type CellEdit = Option<(EntityId, String, String)>;

fn header(ui: &mut egui::Ui, id: &str, t: &Theme) {
    ui.add_space(2.0);
    ui.label(
        RichText::new(tr(id).to_uppercase())
            .small()
            .color(t.text_weak),
    );
}

/// Root › … › active map, right of the menus.
pub fn breadcrumb(ui: &mut egui::Ui, maps: &[Map], active: MapId, out: &mut Vec<Intent>) {
    let path = workspace::breadcrumb(maps, active);
    for (i, id) in path.iter().enumerate() {
        if i > 0 {
            ui.weak("›");
        }
        let label = workspace::map_label(maps, *id);
        if *id == active {
            ui.strong(label);
        } else if ui.link(label).clicked() {
            out.push(Intent::OpenMap(*id));
        }
    }
}

pub fn mode_switch(ui: &mut egui::Ui, mode: Mode, has_generators: bool, out: &mut Vec<Intent>) {
    // Right-to-left layout: Generate first so Edit sits left of it.
    ui.add_enabled_ui(has_generators, |ui| {
        if ui
            .selectable_label(mode == Mode::Generate, tr("mode-generate"))
            .clicked()
        {
            out.push(Intent::SetMode(Mode::Generate));
        }
    });
    if ui
        .selectable_label(mode == Mode::Edit, tr("mode-edit"))
        .clicked()
    {
        out.push(Intent::SetMode(Mode::Edit));
    }
}

pub fn tab_bar(
    ui: &mut egui::Ui,
    tabs: &Tabs,
    maps: &[Map],
    entities: &[Entity],
    out: &mut Vec<Intent>,
) {
    ui.horizontal(|ui| {
        for (i, tab) in tabs.items.iter().enumerate() {
            let title = match tab {
                Tab::Map(m) => tr_args("tab-map", &[("name", &workspace::map_label(maps, *m))]),
                Tab::Page(e) => tr_args(
                    "tab-page",
                    &[(
                        "name",
                        entities
                            .iter()
                            .find(|x| x.id == *e)
                            .map_or("", |x| x.name.as_str()),
                    )],
                ),
            };
            if ui.selectable_label(i == tabs.active, title).clicked() {
                out.push(Intent::ActivateTab(i));
            }
            if tabs.items.len() > 1 && ui.small_button("×").clicked() {
                out.push(Intent::CloseTab(i));
            }
            ui.add_space(6.0);
        }
    });
}

pub fn outliner(
    ui: &mut egui::Ui,
    rows: &[Row],
    selected: Option<EntityId>,
    children: &[(MapId, String)],
    t: &Theme,
    out: &mut Vec<Intent>,
) {
    header(ui, "panel-outliner", t);
    if rows.is_empty() {
        ui.weak(tr("outliner-empty"));
    }
    egui::ScrollArea::vertical()
        .id_salt("outliner")
        .max_height(240.0)
        .show(ui, |ui| {
            for r in rows {
                ui.horizontal(|ui| {
                    ui.add_space(r.depth as f32 * 12.0);
                    if ui
                        .selectable_label(selected == Some(r.id), &r.name)
                        .clicked()
                    {
                        out.push(Intent::Select(Some(r.id)));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(&r.kind);
                    });
                });
            }
        });
    if !children.is_empty() {
        header(ui, "panel-child-maps", t);
        for (id, label) in children {
            if ui.link(label).clicked() {
                out.push(Intent::OpenMap(*id));
            }
        }
    }
}

/// What the properties panel needs about the selection.
pub struct PropsInput<'a> {
    pub entity: &'a Entity,
    pub defs: &'a [FieldDef],
    pub entities: &'a [Entity],
    /// A child map can be made from the entity on this map.
    pub can_child: bool,
}

pub fn properties(
    ui: &mut egui::Ui,
    input: Option<PropsInput>,
    edit: &mut CellEdit,
    t: &Theme,
    out: &mut Vec<Intent>,
) {
    header(ui, "panel-properties", t);
    let Some(i) = input else {
        ui.weak(tr("properties-none"));
        return;
    };
    let e = i.entity;
    egui::Grid::new("fields")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label(tr("field-name"));
            let current = e.name.clone();
            if let Some(text) = cell(ui, edit, e.id, "\0name", &current)
                && !text.trim().is_empty()
                && text != current
            {
                out.push(Intent::Rename {
                    id: e.id,
                    name: text,
                });
            }
            ui.end_row();
            for d in i.defs {
                ui.label(if d.builtin {
                    tr(&d.label)
                } else {
                    d.label.clone()
                });
                let v = e.fields.get(&d.key);
                field_editor(ui, edit, e, d, v, i.entities, out);
                ui.end_row();
            }
        });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button(tr("properties-open-page")).clicked() {
            out.push(Intent::OpenPage(e.id));
        }
        if i.can_child && ui.button(tr("properties-create-child")).clicked() {
            out.push(Intent::CreateChild(e.id));
        }
    });
}

/// A text cell. Returns the text when the user finished typing in it (Enter or focus lost).
fn cell(
    ui: &mut egui::Ui,
    edit: &mut CellEdit,
    id: EntityId,
    key: &str,
    current: &str,
) -> Option<String> {
    let mine = matches!(edit, Some((i, k, _)) if *i == id && k == key);
    let mut text = match edit {
        Some((_, _, t)) if mine => t.clone(),
        _ => current.to_owned(),
    };
    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(150.0));
    if r.lost_focus() {
        *edit = None;
        return Some(text);
    }
    if r.has_focus() {
        *edit = Some((id, key.to_owned(), text));
    }
    None
}

fn field_editor(
    ui: &mut egui::Ui,
    edit: &mut CellEdit,
    e: &Entity,
    d: &FieldDef,
    v: Option<&Value>,
    entities: &[Entity],
    out: &mut Vec<Intent>,
) {
    let set = |out: &mut Vec<Intent>, value| {
        out.push(Intent::SetField {
            id: e.id,
            key: d.key.clone(),
            value,
        })
    };
    match &d.field_type {
        FieldType::YesNo => {
            let mut b = v.and_then(Value::as_bool).unwrap_or(false);
            if ui.checkbox(&mut b, "").changed() {
                set(out, Value::Bool(b));
            }
        }
        FieldType::Choice(options) => {
            let shown = workspace::format_field(&d.field_type, v);
            egui::ComboBox::from_id_salt(("choice", e.id, &d.key))
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(v.is_none(), "—").clicked() {
                        set(out, Value::Null);
                    }
                    for o in options {
                        if ui
                            .selectable_label(v.and_then(Value::as_str) == Some(o), o)
                            .clicked()
                        {
                            set(out, Value::from(o.as_str()));
                        }
                    }
                });
        }
        FieldType::EntityRef => match workspace::ref_target(entities, v) {
            Some(target) => {
                let name = entities
                    .iter()
                    .find(|x| x.id == target)
                    .map_or("", |x| x.name.as_str());
                ui.horizontal(|ui| {
                    if ui.link(name).clicked() {
                        out.push(Intent::Select(Some(target)));
                    }
                    if ui.small_button("×").clicked() {
                        set(out, Value::Null);
                    }
                });
            }
            None => {
                ui.weak("—");
            }
        },
        ty => {
            let current = workspace::format_field(ty, v);
            if let Some(text) = cell(ui, edit, e.id, &d.key, &current)
                && text != current
            {
                match workspace::parse_field(ty, &text) {
                    Ok(value) => set(out, value),
                    Err(_) => {
                        // Keep what the user typed so they can fix it.
                        *edit = Some((e.id, d.key.clone(), text));
                    }
                }
            }
            if let Some((i, k, text)) = edit
                && *i == e.id
                && *k == d.key
                && workspace::parse_field(ty, text).is_err()
            {
                ui.colored_label(ui.visuals().error_fg_color, tr("field-invalid"));
            }
        }
    }
}

pub fn layers(
    ui: &mut egui::Ui,
    kind: Option<&dyn MapKindModule>,
    hidden: &HashSet<Layer>,
    t: &Theme,
    out: &mut Vec<Intent>,
) {
    header(ui, "panel-layers", t);
    let Some(k) = kind else {
        return;
    };
    for &l in k.layers() {
        let drawn = k.render_passes().iter().any(|p| p.layer == l);
        let derived = k.stages().iter().any(|s| s.writes.contains(&l));
        let mut on = !hidden.contains(&l);
        let name = tr(&format!("layer-{}", l.as_str()))
            + &if derived {
                format!(" {}", tr("layer-derived"))
            } else {
                String::new()
            };
        let r = ui.add_enabled(drawn, egui::Checkbox::new(&mut on, name));
        if r.changed() {
            out.push(Intent::ToggleLayer(l));
        }
    }
}

/// Generate mode: the kind's generators, the seed, the last run, Generate and Regenerate selection.
pub struct GenInput<'a> {
    pub gens: Vec<&'static GeneratorDecl>,
    pub generator: &'a mut usize,
    pub seed: &'a mut u64,
    /// Parameter values by id; missing ones show the default.
    pub params: &'a mut Vec<(String, f64)>,
    pub busy: bool,
    pub last: Option<String>,
    /// The selected entity's outline on this map: the area "Regenerate selection" uses.
    pub selection: Option<Vec<RootPoint>>,
}

pub fn generate_panel(ui: &mut egui::Ui, g: GenInput, t: &Theme, out: &mut Vec<Intent>) {
    header(ui, "panel-generate", t);
    let gens = g.gens;
    *g.generator = (*g.generator).min(gens.len().saturating_sub(1));
    for (i, d) in gens.iter().enumerate() {
        ui.radio_value(g.generator, i, tr(&format!("generator-{}", d.id)));
    }
    ui.horizontal(|ui| {
        ui.label(tr("generate-seed"));
        ui.add(egui::DragValue::new(g.seed));
    });
    if let Some(d) = gens.get(*g.generator) {
        for p in d.params {
            let at = match g.params.iter().position(|(id, _)| id == p.id) {
                Some(i) => i,
                None => {
                    g.params.push((p.id.into(), p.default));
                    g.params.len() - 1
                }
            };
            ui.horizontal(|ui| {
                ui.label(tr(&format!("param-{}", p.id)));
                ui.add(egui::DragValue::new(&mut g.params[at].1).range(p.min..=p.max));
            });
        }
    }
    if let Some(last) = g.last {
        ui.weak(last);
    }
    ui.add_space(4.0);
    let run = gens.get(*g.generator).map(|d| GenRun {
        generator: d.id.into(),
        seed: *g.seed,
        params: g
            .params
            .iter()
            .filter(|(id, _)| d.params.iter().any(|p| p.id == id))
            .cloned()
            .collect(),
    });
    ui.add_enabled_ui(!g.busy && run.is_some(), |ui| {
        if ui.button(tr("generate-run")).clicked()
            && let Some(run) = run.clone()
        {
            out.push(Intent::Generate(run));
        }
    });
    let can_regen = !g.busy && run.is_some() && g.selection.is_some();
    let r = ui
        .add_enabled(can_regen, egui::Button::new(tr("generate-regenerate")))
        .on_disabled_hover_text(tr("generate-regenerate-unavailable"));
    if r.clicked()
        && let (Some(run), Some(sel)) = (run, g.selection)
    {
        out.push(Intent::RegenerateSelection(run, sel));
    }
    if g.busy {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.weak(tr("generate-busy"));
        });
    }
}

/// F3 window state.
#[derive(Default)]
pub struct Search {
    pub open: bool,
    pub query: String,
    pub focus: bool,
}

pub enum Pick {
    Command(AppCommand),
    Entity(EntityId),
}

pub fn command_search(
    ctx: &egui::Context,
    s: &mut Search,
    commands: &[AppCommand],
    names: &[(EntityId, String)],
) -> Option<Pick> {
    let mut pick = None;
    let mut open = s.open;
    egui::Window::new(tr("search-title"))
        .open(&mut open)
        .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut s.query)
                    .hint_text(tr("search-hint"))
                    .desired_width(320.0),
            );
            if s.focus {
                r.request_focus();
                s.focus = false;
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let mut first = None;
            for c in commands {
                let id = workspace::COMMANDS
                    .iter()
                    .find(|x| x.0 == *c)
                    .map_or("", |x| x.1);
                first.get_or_insert(Pick::Command(*c));
                if ui.selectable_label(false, tr(id)).clicked() {
                    pick = Some(Pick::Command(*c));
                }
            }
            for (id, name) in names {
                first.get_or_insert(Pick::Entity(*id));
                if ui.selectable_label(false, name).clicked() {
                    pick = Some(Pick::Entity(*id));
                }
            }
            if enter {
                pick = pick.take().or(first);
            }
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) || pick.is_some() {
        open = false;
    }
    s.open = open;
    pick
}

/// Preview of a masked regeneration: what it rewrites and keeps. `Some(true)` applies it.
pub fn regen_preview(ctx: &egui::Context, plan: &RegenPlan) -> Option<bool> {
    let mut answer = None;
    egui::Window::new(tr("regen-title"))
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            let n = |v: &[mmc_core::TileAt]| v.len().to_string();
            ui.label(tr_args(
                "regen-summary",
                &[
                    ("tiles", &n(&plan.regenerate)),
                    ("edited", &n(&plan.kept_edited)),
                    ("locked", &n(&plan.kept_locked)),
                ],
            ));
            ui.horizontal(|ui| {
                if ui.button(tr("regen-apply")).clicked() {
                    answer = Some(true);
                }
                if ui.button(tr("regen-cancel")).clicked() {
                    answer = Some(false);
                }
            });
        });
    answer
}

/// Offer for a working copy a crash left behind.
pub fn restore_prompt(ctx: &egui::Context) -> Option<bool> {
    let mut answer = None;
    egui::Window::new(tr("restore-title"))
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(tr("restore-text"));
            ui.horizontal(|ui| {
                if ui.button(tr("restore-restore")).clicked() {
                    answer = Some(true);
                }
                if ui.button(tr("restore-discard")).clicked() {
                    answer = Some(false);
                }
            });
        });
    answer
}
