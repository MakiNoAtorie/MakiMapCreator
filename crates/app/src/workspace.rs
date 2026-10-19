//! Workspace logic without drawing: outliner tree, field values, breadcrumb, tabs, modes, command search.

use std::collections::HashMap;

use mmc_core::{Entity, EntityId, EntityKind, FieldType, Map, MapId, Relation, RootPoint};
use serde_json::Value;

/// Relation kind that makes the outliner tree: `from` is part of `to`.
const PART_OF: &str = "part_of";

/// One outliner line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: EntityId,
    pub depth: usize,
    pub name: String,
    /// Kind label, shown right-aligned.
    pub kind: String,
}

/// Entities as a tree by `part_of`; roots and siblings in id order. An entity in a cycle is a root.
pub fn outliner_rows(
    entities: &[Entity],
    relations: &[Relation],
    kinds: &[EntityKind],
) -> Vec<Row> {
    let by_id: HashMap<EntityId, &Entity> = entities.iter().map(|e| (e.id, e)).collect();
    let mut parent: HashMap<EntityId, EntityId> = HashMap::new();
    for r in relations.iter().filter(|r| r.kind == PART_OF) {
        if by_id.contains_key(&r.from) && by_id.contains_key(&r.to) {
            parent.entry(r.from).or_insert(r.to);
        }
    }
    // Break cycles: walk up; if we return to the start, the start becomes a root.
    for e in entities {
        let mut at = e.id;
        for _ in 0..=entities.len() {
            match parent.get(&at) {
                Some(&p) if p == e.id => {
                    parent.remove(&e.id);
                    break;
                }
                Some(&p) => at = p,
                None => break,
            }
        }
    }
    let mut children: HashMap<Option<EntityId>, Vec<EntityId>> = HashMap::new();
    for e in entities {
        children
            .entry(parent.get(&e.id).copied())
            .or_default()
            .push(e.id);
    }
    let label = |k: &str| {
        kinds
            .iter()
            .find(|x| x.id == k)
            .map_or(k.to_owned(), |x| x.label.clone())
    };
    let mut rows = vec![];
    let mut stack: Vec<(EntityId, usize)> = children
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|&i| (i, 0))
        .collect();
    while let Some((id, depth)) = stack.pop() {
        let e = by_id[&id];
        rows.push(Row {
            id,
            depth,
            name: e.name.clone(),
            kind: label(&e.kind),
        });
        stack.extend(
            children
                .get(&Some(id))
                .into_iter()
                .flatten()
                .rev()
                .map(|&c| (c, depth + 1)),
        );
    }
    rows
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError;

/// Parses text typed into a field grid cell. Empty input clears the field (`Null`).
pub fn parse_field(ty: &FieldType, input: &str) -> Result<Value, FieldError> {
    let t = input.trim();
    match ty {
        FieldType::Text | FieldType::Markdown => Ok(if input.is_empty() {
            Value::Null
        } else {
            Value::from(input)
        }),
        _ if t.is_empty() => Ok(Value::Null),
        FieldType::Number => {
            let n: f64 = t.parse().map_err(|_| FieldError)?;
            if !n.is_finite() {
                return Err(FieldError);
            }
            Ok(if n.fract() == 0.0 && n.abs() < 9e15 {
                Value::from(n as i64)
            } else {
                Value::from(n)
            })
        }
        FieldType::YesNo => match t.to_lowercase().as_str() {
            "yes" | "true" => Ok(Value::Bool(true)),
            "no" | "false" => Ok(Value::Bool(false)),
            _ => Err(FieldError),
        },
        FieldType::Choice(options) => options
            .iter()
            .find(|o| o.as_str() == t)
            .map(|o| Value::from(o.as_str()))
            .ok_or(FieldError),
        // References are chosen by link, not typed.
        FieldType::EntityRef => Err(FieldError),
    }
}

/// Text for a field cell; empty when unset. Entity references show as the id (the UI resolves names).
pub fn format_field(ty: &FieldType, v: Option<&Value>) -> String {
    match (ty, v) {
        (_, None | Some(Value::Null)) => String::new(),
        (FieldType::YesNo, Some(Value::Bool(b))) => if *b { "yes" } else { "no" }.into(),
        (_, Some(Value::String(s))) => s.clone(),
        (_, Some(Value::Number(n))) => match n.as_f64() {
            Some(f) if n.is_f64() && f.fract() == 0.0 => format!("{f:.0}"),
            _ => n.to_string(),
        },
        (_, Some(other)) => other.to_string(),
    }
}

/// The entity an entity-reference value points at.
pub fn ref_target(entities: &[Entity], v: Option<&Value>) -> Option<EntityId> {
    let n = v?.as_i64()?;
    entities
        .iter()
        .find(|e| Value::from(e.id).as_i64() == Some(n))
        .map(|e| e.id)
}

/// Maps from `active` up to its root, root first.
pub fn breadcrumb(maps: &[Map], active: MapId) -> Vec<MapId> {
    let mut path = vec![];
    let mut at = Some(active);
    while let Some(id) = at {
        if path.contains(&id) {
            break;
        }
        path.push(id);
        at = maps.iter().find(|m| m.id == id).and_then(|m| m.parent);
    }
    path.reverse();
    path
}

/// "Region map", or "Region map 2" when the project has several of the kind (maps have no names yet).
pub fn map_label(maps: &[Map], id: MapId) -> String {
    let Some(m) = maps.iter().find(|m| m.id == id) else {
        return String::new();
    };
    let same: Vec<_> = maps
        .iter()
        .filter(|x| x.settings.kind == m.settings.kind)
        .collect();
    let base = crate::status::map_kind(m.settings.kind);
    match same.iter().position(|x| x.id == id) {
        Some(i) if same.len() > 1 => format!("{base} {}", i + 1),
        _ => base,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Map(MapId),
    Page(EntityId),
}

/// Open tabs. There is always at least one when a project is open.
#[derive(Debug, Default)]
pub struct Tabs {
    pub items: Vec<Tab>,
    pub active: usize,
}

impl Tabs {
    pub fn only(tab: Tab) -> Tabs {
        Tabs {
            items: vec![tab],
            active: 0,
        }
    }

    pub fn active(&self) -> Option<Tab> {
        self.items.get(self.active).copied()
    }

    /// Shows `tab`, opening it if needed.
    pub fn open(&mut self, tab: Tab) {
        match self.items.iter().position(|t| *t == tab) {
            Some(i) => self.active = i,
            None => {
                self.items.push(tab);
                self.active = self.items.len() - 1;
            }
        }
    }

    /// Closes tab `i`; the last tab stays.
    pub fn close(&mut self, i: usize) {
        if self.items.len() < 2 || i >= self.items.len() {
            return;
        }
        self.items.remove(i);
        if i < self.active || self.active == self.items.len() {
            self.active -= 1;
        }
    }

    /// Drops tabs whose map or page `keep` rejects.
    pub fn retain(&mut self, keep: impl Fn(&Tab) -> bool) {
        let cur = self.active();
        self.items.retain(|t| keep(t));
        self.active = cur
            .and_then(|c| self.items.iter().position(|t| *t == c))
            .unwrap_or(0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Edit,
    Generate,
}

impl Mode {
    /// A kind with no generators has no Generate mode.
    pub fn coerce(self, has_generators: bool) -> Mode {
        if has_generators { self } else { Mode::Edit }
    }
}

/// Commands the search lists, besides entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    New,
    Open,
    Save,
    SaveAs,
    Fit,
    EditMode,
    GenerateMode,
    TogglePanels,
}

/// (command, UI string id).
pub const COMMANDS: &[(AppCommand, &str)] = &[
    (AppCommand::New, "menu-file-new"),
    (AppCommand::Open, "menu-file-open"),
    (AppCommand::Save, "menu-file-save"),
    (AppCommand::SaveAs, "menu-file-save-as"),
    (AppCommand::Fit, "menu-view-fit"),
    (AppCommand::EditMode, "mode-edit"),
    (AppCommand::GenerateMode, "mode-generate"),
    (AppCommand::TogglePanels, "menu-view-panels"),
];

/// Commands whose label contains `query` (case-insensitive); prefix matches first, then list order.
// ponytail: substring match; core's name scorer is not exposed for commands.
pub fn rank_commands(query: &str, labels: &[(AppCommand, String)]) -> Vec<AppCommand> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<(bool, usize, AppCommand)> = labels
        .iter()
        .enumerate()
        .filter_map(|(i, (c, l))| {
            let l = l.to_lowercase();
            l.contains(&q).then_some((!l.starts_with(&q), i, *c))
        })
        .collect();
    hits.sort_by_key(|h| (h.0, h.1));
    hits.into_iter().map(|h| h.2).collect()
}

/// Even-odd point in polygon.
fn inside(poly: &[RootPoint], p: RootPoint) -> bool {
    let mut odd = false;
    for (a, b) in poly.iter().zip(poly.iter().cycle().skip(1)) {
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            odd = !odd;
        }
    }
    odd
}

/// The entity drawn last whose outline contains `p`.
pub fn pick(outlines: &[(EntityId, Vec<RootPoint>)], p: RootPoint) -> Option<EntityId> {
    outlines.iter().rev().find(|o| inside(&o.1, p)).map(|o| o.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mmc_core::{Command, EntityEdit, MapKind, MapSettings, Project};
    use serde_json::Map as JsonMap;

    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let store = mmc_store::Store::create(&dir.path().join("w.mmc")).unwrap();
        let p = Project::create(Box::new(store), MapSettings::default_for(MapKind::World)).unwrap();
        (dir, p)
    }

    fn add(p: &mut Project, kind: &str, name: &str) -> EntityId {
        let id = p.next_entity_id().unwrap();
        let map = p.maps()[0].id;
        let e = Entity {
            id,
            kind: kind.into(),
            name: name.into(),
            fields: JsonMap::new(),
        };
        p.apply(map, Command::Entity(EntityEdit::Create(e)))
            .unwrap();
        id
    }

    fn part_of(from: EntityId, to: EntityId) -> Relation {
        Relation {
            from,
            to,
            kind: PART_OF.into(),
        }
    }

    #[test]
    fn outliner_nests_parts_under_wholes() {
        let (_d, mut p) = project();
        let a = add(&mut p, "region", "Alpha");
        let b = add(&mut p, "region", "Beta");
        let c = add(&mut p, "region", "Gamma");
        let rows = outliner_rows(
            &p.entities().unwrap(),
            &[part_of(c, a)],
            &p.entity_kinds().unwrap(),
        );
        let shape: Vec<_> = rows.iter().map(|r| (r.id, r.depth)).collect();
        assert_eq!(shape, vec![(a, 0), (c, 1), (b, 0)]);
        assert_eq!(rows[0].kind, "Region");
    }

    #[test]
    fn outliner_survives_a_cycle() {
        let (_d, mut p) = project();
        let a = add(&mut p, "region", "A");
        let b = add(&mut p, "region", "B");
        let rows = outliner_rows(
            &p.entities().unwrap(),
            &[part_of(a, b), part_of(b, a)],
            &p.entity_kinds().unwrap(),
        );
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn numbers_parse_and_format() {
        let n = FieldType::Number;
        assert_eq!(parse_field(&n, " 12 "), Ok(Value::from(12)));
        assert_eq!(parse_field(&n, "1.5"), Ok(Value::from(1.5)));
        assert_eq!(parse_field(&n, ""), Ok(Value::Null));
        assert_eq!(parse_field(&n, "x"), Err(FieldError));
        assert_eq!(parse_field(&n, "inf"), Err(FieldError));
        assert_eq!(format_field(&n, Some(&Value::from(12))), "12");
        assert_eq!(format_field(&n, Some(&Value::from(1.5))), "1.5");
        assert_eq!(format_field(&n, Some(&Value::from(3.0))), "3");
        assert_eq!(format_field(&n, None), "");
    }

    #[test]
    fn text_choice_and_yes_no_fields() {
        let t = FieldType::Text;
        assert_eq!(parse_field(&t, "hi "), Ok(Value::from("hi ")));
        assert_eq!(parse_field(&t, ""), Ok(Value::Null));
        let c = FieldType::Choice(vec!["a".into(), "b".into()]);
        assert_eq!(parse_field(&c, "b"), Ok(Value::from("b")));
        assert_eq!(parse_field(&c, "z"), Err(FieldError));
        let y = FieldType::YesNo;
        assert_eq!(parse_field(&y, "Yes"), Ok(Value::Bool(true)));
        assert_eq!(parse_field(&y, "maybe"), Err(FieldError));
        assert_eq!(format_field(&y, Some(&Value::Bool(false))), "no");
        assert_eq!(parse_field(&FieldType::EntityRef, "3"), Err(FieldError));
    }

    #[test]
    fn reference_resolves_to_an_entity() {
        let (_d, mut p) = project();
        let a = add(&mut p, "region", "A");
        let es = p.entities().unwrap();
        assert_eq!(ref_target(&es, Some(&Value::from(a))), Some(a));
        assert_eq!(ref_target(&es, Some(&Value::from(999))), None);
        assert_eq!(ref_target(&es, None), None);
    }

    #[test]
    fn breadcrumb_runs_root_to_active() {
        let (_d, mut p) = project();
        let root = p.maps()[0].id;
        let other = p
            .add_root_map(MapSettings::default_for(MapKind::Region))
            .unwrap();
        assert_eq!(breadcrumb(p.maps(), root), vec![root]);
        assert_eq!(breadcrumb(p.maps(), other), vec![other]);
        assert_eq!(map_label(p.maps(), root), "World map");
    }

    #[test]
    fn tabs_open_activate_and_close() {
        let (_d, mut p) = project();
        let (a, b) = (
            p.maps()[0].id,
            p.add_root_map(MapSettings::default_for(MapKind::Region))
                .unwrap(),
        );
        let mut t = Tabs::only(Tab::Map(a));
        t.open(Tab::Map(b));
        t.open(Tab::Map(a));
        assert_eq!((t.items.len(), t.active), (2, 0));
        t.open(Tab::Map(b));
        t.close(1);
        assert_eq!((t.active(), t.items.len()), (Some(Tab::Map(a)), 1));
        t.close(0);
        assert_eq!(t.items.len(), 1);
    }

    #[test]
    fn closing_an_earlier_tab_keeps_the_active_one() {
        let (_d, mut p) = project();
        let (a, b) = (
            p.maps()[0].id,
            p.add_root_map(MapSettings::default_for(MapKind::Region))
                .unwrap(),
        );
        let c = add(&mut p, "region", "C");
        let mut t = Tabs::only(Tab::Map(a));
        t.open(Tab::Map(b));
        t.open(Tab::Page(c));
        t.close(0);
        assert_eq!(t.active(), Some(Tab::Page(c)));
        t.retain(|x| *x != Tab::Page(c));
        assert_eq!(t.active(), Some(Tab::Map(b)));
    }

    #[test]
    fn generate_mode_needs_generators() {
        assert_eq!(Mode::Generate.coerce(false), Mode::Edit);
        assert_eq!(Mode::Generate.coerce(true), Mode::Generate);
    }

    #[test]
    fn commands_rank_prefix_before_substring() {
        let labels = vec![
            (AppCommand::SaveAs, "Save as…".to_owned()),
            (AppCommand::Fit, "Zoom to fit".to_owned()),
            (AppCommand::Save, "Save".to_owned()),
            (AppCommand::Open, "Open…".to_owned()),
        ];
        assert_eq!(
            rank_commands("sa", &labels),
            vec![AppCommand::SaveAs, AppCommand::Save]
        );
        assert_eq!(rank_commands("FIT", &labels), vec![AppCommand::Fit]);
        assert_eq!(rank_commands("", &labels).len(), 4);
        assert!(rank_commands("zzz", &labels).is_empty());
    }

    #[test]
    fn pick_takes_the_last_containing_outline() {
        let (_d, mut p) = project();
        let (a, b) = (add(&mut p, "region", "A"), add(&mut p, "region", "B"));
        let sq = |s: f64| {
            vec![
                RootPoint { x: 0.0, y: 0.0 },
                RootPoint { x: s, y: 0.0 },
                RootPoint { x: s, y: s },
                RootPoint { x: 0.0, y: s },
            ]
        };
        let o = vec![(a, sq(10.0)), (b, sq(4.0))];
        let at = |x| RootPoint { x, y: 2.0 };
        assert_eq!(pick(&o, at(3.0)), Some(b));
        assert_eq!(pick(&o, at(7.0)), Some(a));
        assert_eq!(pick(&o, at(11.0)), None);
    }
}
