//! Deletion rules (seam 1): deleting a map keeps lore, child maps become root maps, and an
//! entity with its own map needs a choice. Both deletions undo fully.

use mmc_core::{
    Command, Entity, EntityEdit, EntityId, Error, Generation, Kinds, Layer, MapId, MapKind,
    MapKindModule, MapSettings, NewEntity, OwnMap, PageEdit, PaintStroke, Project, RootPoint,
};

struct TestKind(MapKind, Option<MapKind>);

impl MapKindModule for TestKind {
    fn kind(&self) -> MapKind {
        self.0
    }
    fn layers(&self) -> &'static [Layer] {
        &[Layer::Land]
    }
    fn render_layers(&self) -> &'static [Layer] {
        &[]
    }
    fn tools(&self) -> &'static [mmc_core::Tool] {
        &[]
    }
    fn generators(&self) -> &'static [&'static str] {
        &[]
    }
    fn generate(&self, _: &Project, _: MapId, g: &str, _: u64) -> mmc_core::Result<Command> {
        Err(Error::UnknownGenerator(g.into()))
    }
    fn entity_kinds(&self) -> &'static [&'static str] {
        &[]
    }
    fn child_kind(&self) -> Option<MapKind> {
        self.1
    }
}

fn kinds() -> Kinds {
    Kinds::new(vec![
        Box::new(TestKind(MapKind::World, Some(MapKind::Region))),
        Box::new(TestKind(MapKind::Region, Some(MapKind::Settlement))),
    ])
}

fn at(x: f64, y: f64) -> RootPoint {
    RootPoint { x, y }
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<RootPoint> {
    vec![at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1)]
}

/// Adds one entity with `outline` on `map` and returns its id.
fn outline_entity(p: &mut Project, map: MapId, kind: &str, outline: Vec<RootPoint>) -> EntityId {
    let id = p.next_entity_id().unwrap();
    let g = Generation {
        generator: "test".into(),
        seed: 1,
        entities: vec![NewEntity {
            kind: kind.into(),
            name: format!("{kind} {map:?}"),
            outline,
        }],
        rasters: vec![],
    };
    p.apply(map, Command::Generate(g)).unwrap();
    id
}

fn paint(p: &mut Project, map: MapId, c: RootPoint, value: f32) {
    let s = PaintStroke {
        layer: Layer::Land,
        points: vec![c],
        radius: 50.0,
        value,
    };
    p.apply(map, Command::Paint(s)).unwrap();
}

fn land(p: &Project, map: MapId, c: RootPoint) -> Option<f32> {
    p.layer_value(map, Layer::Land, c).unwrap()
}

struct Tree {
    p: Project,
    /// World (root) -> region map from `region` -> settlement map from `town`.
    world: MapId,
    region_map: MapId,
    town_map: MapId,
    region: EntityId,
    town: EntityId,
}

fn tree(dir: &std::path::Path) -> Tree {
    let k = kinds();
    let world = MapSettings {
        kind: MapKind::World,
        width: 32,
        height: 32,
        metres_per_pixel: 1000.0,
    };
    let mut p = Project::create(&dir.join("wc.sqlite"), world).unwrap();
    let w = p.maps()[0].id;
    let region = outline_entity(&mut p, w, "region", rect(4e3, 4e3, 12e3, 12e3));
    let r = p.create_child_map(&k, w, region, None).unwrap();
    let town = outline_entity(&mut p, r, "settlement", rect(6e3, 6e3, 6.2e3, 6.2e3));
    let t = p.create_child_map(&k, r, town, None).unwrap();
    paint(&mut p, r, at(8e3, 8e3), 0.5);
    paint(&mut p, t, at(6.1e3, 6.1e3), 0.75);
    let edit = PageEdit {
        entity: region,
        text: "The [[1|old]] realm.".into(),
    };
    p.apply(w, Command::Page(edit)).unwrap();
    Tree {
        p,
        world: w,
        region_map: r,
        town_map: t,
        region,
        town,
    }
}

fn map(p: &Project, id: MapId) -> mmc_core::Map {
    p.maps().iter().find(|m| m.id == id).unwrap().clone()
}

#[test]
fn deleting_a_map_keeps_lore_turns_children_into_roots_and_undoes_fully() {
    let dir = tempfile::tempdir().unwrap();
    let Tree {
        mut p,
        world,
        region_map: r,
        town_map: t,
        region,
        town,
    } = tree(dir.path());
    let before = p.maps().to_vec();
    let town_outlines = p.outlines(r).unwrap();
    let page = p.page(region).unwrap();
    let town_before = map(&p, t);

    p.delete_map(r).unwrap();

    assert!(p.maps().iter().all(|m| m.id != r));
    assert!(matches!(p.outlines(r), Err(Error::UnknownMap(_))));
    assert!(matches!(p.undo(r), Err(Error::UnknownMap(_))));
    // Entities and pages stay.
    assert!(p.entity(town).unwrap().is_some());
    assert_eq!(p.page(region).unwrap(), page);
    // The child keeps its origin and tiles; its parent link and snapshot are gone.
    let t_now = map(&p, t);
    assert_eq!((t_now.parent, t_now.outline.clone()), (None, None));
    assert_eq!(t_now.origin, town_before.origin);
    assert_eq!(t_now.settings, town_before.settings);
    assert_eq!(land(&p, t, at(6.1e3, 6.1e3)), Some(0.75));
    assert_eq!(p.parent_context(t, Layer::Land).unwrap(), None);
    // A root map owns all its ground, so edits outside the old outline now apply.
    paint(&mut p, t, at(5.9e3, 5.9e3), 0.25);
    p.undo(t).unwrap();

    // Undo lives in the parent map's history.
    assert!(p.undo(world).unwrap());
    assert_eq!(p.maps(), &before[..]);
    assert_eq!(p.outlines(r).unwrap(), town_outlines);
    assert_eq!(land(&p, r, at(8e3, 8e3)), Some(0.5));
    assert!(p.parent_context(t, Layer::Land).unwrap().is_some());
    // The deleted map's own history comes back with it.
    assert!(p.undo(r).unwrap());
    assert_eq!(land(&p, r, at(8e3, 8e3)), Some(0.0));
    assert!(p.redo(r).unwrap());

    assert!(p.redo(world).unwrap());
    assert!(p.maps().iter().all(|m| m.id != r));
}

#[test]
fn deleting_a_root_map_undoes_from_the_project_history() {
    let dir = tempfile::tempdir().unwrap();
    let Tree {
        mut p,
        world,
        region_map: r,
        ..
    } = tree(dir.path());
    let before = p.maps().to_vec();
    assert!(!p.undo_project().unwrap());

    p.delete_map(world).unwrap();
    assert!(p.maps().iter().all(|m| m.id != world));
    assert_eq!(map(&p, r).parent, None);

    assert!(p.undo_project().unwrap());
    assert_eq!(p.maps(), &before[..]);
    assert!(p.redo_project().unwrap());
    assert_eq!(p.maps().len(), before.len() - 1);
}

#[test]
fn deleting_an_entity_with_its_own_map_needs_a_choice() {
    let dir = tempfile::tempdir().unwrap();
    let Tree {
        mut p,
        world,
        region_map: r,
        town_map: t,
        region,
        ..
    } = tree(dir.path());
    let before = p.maps().to_vec();

    let plain = p.apply(world, Command::Entity(EntityEdit::Delete(region)));
    assert!(matches!(plain, Err(Error::OwnMapChoice(e)) if e == region));
    assert!(p.entity(region).unwrap().is_some());

    let delete = |own_map| {
        Command::Entity(EntityEdit::DeleteWithMap {
            id: region,
            own_map,
        })
    };
    p.apply(world, delete(OwnMap::Delete)).unwrap();
    assert!(p.entity(region).unwrap().is_none());
    assert!(p.maps().iter().all(|m| m.id != r));
    assert_eq!(map(&p, t).parent, None);
    // The deleted entity's outline no longer shows on the parent.
    assert!(p.outlines(world).unwrap().is_empty());
    assert!(p.undo(world).unwrap());
    assert_eq!(p.maps(), &before[..]);
    assert!(p.entity(region).unwrap().is_some());
    assert_eq!(p.outlines(world).unwrap().len(), 1);

    p.apply(world, delete(OwnMap::KeepAsRoot)).unwrap();
    assert!(p.entity(region).unwrap().is_none());
    let kept = map(&p, r);
    assert_eq!((kept.parent, kept.outline), (None, None));
    assert_eq!(map(&p, t).parent, Some(r));
    assert_eq!(land(&p, r, at(8e3, 8e3)), Some(0.5));
    assert!(p.undo(world).unwrap());
    assert_eq!(p.maps(), &before[..]);
}

#[test]
fn deleting_an_entity_from_its_own_map_records_undo_on_the_parent() {
    let dir = tempfile::tempdir().unwrap();
    let Tree {
        mut p,
        world,
        region_map: r,
        region,
        ..
    } = tree(dir.path());
    let before = p.maps().to_vec();
    let cmd = Command::Entity(EntityEdit::DeleteWithMap {
        id: region,
        own_map: OwnMap::Delete,
    });
    p.apply(r, cmd).unwrap();
    assert!(p.undo(world).unwrap());
    assert_eq!(p.maps(), &before[..]);
}

#[test]
fn references_to_a_deleted_entity_keep_the_id_and_resolve_as_missing() {
    let dir = tempfile::tempdir().unwrap();
    let Tree {
        mut p, world, town, ..
    } = tree(dir.path());
    // Town has its own map; delete a plain settlement instead.
    let _ = town;
    let capital = p.next_entity_id().unwrap();
    let mk = |id, kind: &str| Entity {
        id,
        kind: kind.into(),
        name: kind.into(),
        fields: Default::default(),
    };
    p.apply(
        world,
        Command::Entity(EntityEdit::Create(mk(capital, "settlement"))),
    )
    .unwrap();
    let nation = p.next_entity_id().unwrap();
    p.apply(
        world,
        Command::Entity(EntityEdit::Create(mk(nation, "nation"))),
    )
    .unwrap();
    let set = EntityEdit::SetField {
        id: nation,
        key: "capital".into(),
        value: capital.into(),
    };
    p.apply(world, Command::Entity(set)).unwrap();

    p.apply(world, Command::Entity(EntityEdit::Delete(capital)))
        .unwrap();
    let field = p.entity(nation).unwrap().unwrap().fields["capital"].clone();
    assert_eq!(field, serde_json::Value::from(capital));
    assert!(p.entity(capital).unwrap().is_none());
    // Other fields of the entity still accept edits.
    let colour = EntityEdit::SetField {
        id: nation,
        key: "colour".into(),
        value: "red".into(),
    };
    p.apply(world, Command::Entity(colour)).unwrap();

    p.undo(world).unwrap();
    p.undo(world).unwrap();
    assert!(p.entity(capital).unwrap().is_some());
}

#[test]
fn a_reopened_project_no_longer_has_the_deleted_map() {
    let dir = tempfile::tempdir().unwrap();
    let Tree {
        mut p,
        region_map: r,
        town_map: t,
        ..
    } = tree(dir.path());
    p.delete_map(r).unwrap();
    let file = dir.path().join("p.mmc");
    p.save(&file);
    p.wait_for_save().unwrap();
    let expected = p.maps().to_vec();
    p.close().unwrap();

    let q = Project::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert_eq!(q.maps(), &expected[..]);
    assert_eq!(map(&q, t).parent, None);
}
