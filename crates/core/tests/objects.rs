//! Map objects (seam 1): stable ids, optional entity, label offset; undoable; gone with their map.

use mmc_core::{
    Command, Entity, EntityEdit, Error, Layer, MapKindModule, MapObject, ObjectEdit, Project,
    RootPoint, Shape,
};
use mmc_kind_region::RegionKind;

fn at(x: f64, y: f64) -> RootPoint {
    RootPoint { x, y }
}

fn project(dir: &std::path::Path) -> Project {
    mmc_fixtures::create(&dir.join("wc.sqlite"), RegionKind.default_settings()).unwrap()
}

fn object(p: &Project, shape: Shape, points: Vec<RootPoint>) -> MapObject {
    MapObject {
        id: p.next_object_id().unwrap(),
        layer: Layer::OUTLINES,
        shape,
        points,
        entity: None,
        label_offset: None,
    }
}

#[test]
fn objects_are_created_updated_and_deleted_as_undoable_commands() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project(dir.path());
    let map = p.maps()[0].id;
    let town = p.next_entity_id().unwrap();
    let e = Entity {
        id: town,
        kind: "settlement".into(),
        name: "Riverford".into(),
        fields: Default::default(),
    };
    p.apply(map, Command::Entity(EntityEdit::Create(e)))
        .unwrap();

    let mut o = object(&p, Shape::Point, vec![at(100.0, 200.0)]);
    o.entity = Some(town);
    o.label_offset = Some((5.0, -12.5));
    let id = o.id;
    p.apply(map, Command::Object(ObjectEdit::Create(o.clone())))
        .unwrap();
    let path = object(&p, Shape::Path, vec![at(0.0, 0.0), at(50.0, 10.0)]);
    assert_ne!(path.id, id);
    p.apply(map, Command::Object(ObjectEdit::Create(path.clone())))
        .unwrap();
    assert_eq!(p.objects(map).unwrap(), vec![o.clone(), path.clone()]);

    let moved = MapObject {
        points: vec![at(110.0, 200.0)],
        ..o.clone()
    };
    p.apply(map, Command::Object(ObjectEdit::Update(moved.clone())))
        .unwrap();
    p.apply(map, Command::Object(ObjectEdit::Delete(path.id)))
        .unwrap();
    assert_eq!(p.objects(map).unwrap(), vec![moved.clone()]);

    assert!(p.undo(map).unwrap());
    assert!(p.undo(map).unwrap());
    assert_eq!(p.objects(map).unwrap(), vec![o.clone(), path.clone()]);
    assert!(p.redo(map).unwrap());
    assert!(p.redo(map).unwrap());
    assert_eq!(p.objects(map).unwrap(), vec![moved.clone()]);

    // The id is stable across save and reload.
    let file = dir.path().join("p.mmc");
    p.save(&file);
    p.close().unwrap();
    let q = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert_eq!(q.objects(map).unwrap(), vec![moved]);
}

#[test]
fn bad_objects_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project(dir.path());
    let map = p.maps()[0].id;
    let mut bad = object(&p, Shape::Polygon, vec![at(0.0, 0.0), at(1.0, 0.0)]);
    let err = p
        .apply(map, Command::Object(ObjectEdit::Create(bad.clone())))
        .unwrap_err();
    assert!(matches!(err, Error::InvalidObject(_)), "{err}");
    bad.shape = Shape::Path;
    bad.layer = Layer::HEIGHT;
    let err = p
        .apply(map, Command::Object(ObjectEdit::Create(bad.clone())))
        .unwrap_err();
    assert!(matches!(err, Error::NotAnObjectLayer(_)), "{err}");
    let err = p
        .apply(map, Command::Object(ObjectEdit::Delete(bad.id)))
        .unwrap_err();
    assert!(matches!(err, Error::UnknownObject(_)), "{err}");
    assert!(!p.undo(map).unwrap());
}

#[test]
fn objects_are_deleted_with_their_map() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project(dir.path());
    let first = p.maps()[0].id;
    let map = p.add_root_map(RegionKind.default_settings()).unwrap();
    let o = object(&p, Shape::Point, vec![at(1.0, 1.0)]);
    p.apply(map, Command::Object(ObjectEdit::Create(o.clone())))
        .unwrap();
    p.delete_map(map).unwrap();
    assert!(matches!(p.objects(map), Err(Error::UnknownMap(_))));
    assert!(p.undo_project().unwrap());
    assert_eq!(p.objects(map).unwrap(), vec![o]);
    assert!(p.redo_project().unwrap());

    let file = dir.path().join("p.mmc");
    p.save(&file);
    p.close().unwrap();
    let q = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert_eq!(q.maps().len(), 1);
    assert!(q.objects(first).unwrap().is_empty());
}

#[test]
fn the_entity_under_a_point_comes_from_objects_then_anchors() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project(dir.path());
    let map = p.maps()[0].id;
    let mut add = |kind: &str| {
        let id = p.next_entity_id().unwrap();
        let e = Entity {
            id,
            kind: kind.into(),
            name: kind.into(),
            fields: Default::default(),
        };
        p.apply(map, Command::Entity(EntityEdit::Create(e)))
            .unwrap();
        id
    };
    let (region, town, river, lake) = (add("region"), add("settlement"), add("river"), add("lake"));
    let mut area = object(
        &p,
        Shape::Polygon,
        vec![
            at(0.0, 0.0),
            at(1000.0, 0.0),
            at(1000.0, 1000.0),
            at(0.0, 1000.0),
        ],
    );
    area.entity = Some(region);
    p.apply(map, Command::Object(ObjectEdit::Create(area)))
        .unwrap();
    let mut dot = object(&p, Shape::Point, vec![at(500.0, 500.0)]);
    dot.entity = Some(town);
    p.apply(map, Command::Object(ObjectEdit::Create(dot)))
        .unwrap();
    let anchors = mmc_core::Anchors::Lake {
        inside: at(3000.0, 3000.0),
    };
    let edit = mmc_core::AnchorEdit::Set {
        entity: lake,
        anchors,
    };
    p.apply(map, Command::Anchor(edit)).unwrap();
    let anchors = mmc_core::Anchors::River {
        mouth: at(5000.0, 0.0),
        picked: at(5000.0, 400.0),
    };
    let edit = mmc_core::AnchorEdit::Set {
        entity: river,
        anchors,
    };
    p.apply(map, Command::Anchor(edit)).unwrap();

    let e = |x, y| p.entity_at(map, at(x, y), 20.0).unwrap();
    assert_eq!(e(510.0, 505.0), Some(town));
    assert_eq!(e(200.0, 200.0), Some(region));
    assert_eq!(e(3010.0, 3000.0), Some(lake));
    assert_eq!(e(5000.0, 410.0), Some(river));
    assert_eq!(e(8000.0, 8000.0), None);
}
