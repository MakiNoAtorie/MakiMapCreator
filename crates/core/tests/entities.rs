use mmc_core::{
    Command, Entity, EntityEdit, EntityId, Error, FieldType, MapKind, MapSettings, Project,
    Relation,
};
use serde_json::{Map, Value, json};

fn new_project(dir: &std::path::Path) -> Project {
    mmc_fixtures::create(
        &dir.join("wc.sqlite"),
        MapSettings::default_for(MapKind::Region),
    )
    .unwrap()
}

fn fields(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn create(p: &mut Project, kind: &str, name: &str, f: Value) -> Result<EntityId, Error> {
    let map = p.maps()[0].id;
    let id = p.next_entity_id()?;
    p.apply(
        map,
        Command::Entity(EntityEdit::Create(Entity {
            id,
            kind: kind.into(),
            name: name.into(),
            fields: fields(f),
        })),
    )?;
    Ok(id)
}

fn edit(p: &mut Project, e: EntityEdit) -> Result<(), Error> {
    let map = p.maps()[0].id;
    p.apply(map, Command::Entity(e))
}

#[test]
fn v1_kinds_and_built_in_fields_are_seeded() {
    let dir = tempfile::tempdir().unwrap();
    let p = new_project(dir.path());
    let kinds: Vec<_> = p
        .entity_kinds()
        .unwrap()
        .into_iter()
        .map(|k| k.id)
        .collect();
    for k in [
        "nation",
        "region",
        "settlement",
        "landmark",
        "dungeon",
        "mountain_range",
        "river",
        "lake",
        "forest",
        "sea",
        "island",
        "desert",
    ] {
        assert!(kinds.iter().any(|x| x == k), "missing kind {k}");
    }
    let pop = p
        .field_defs("settlement")
        .unwrap()
        .into_iter()
        .find(|d| d.key == "population")
        .unwrap();
    assert_eq!(pop.field_type, FieldType::Number);
    assert!(pop.builtin);
    let capital = p
        .field_defs("nation")
        .unwrap()
        .into_iter()
        .find(|d| d.key == "capital")
        .unwrap();
    assert_eq!(capital.field_type, FieldType::EntityRef);
}

#[test]
fn valid_field_values_are_stored_and_invalid_ones_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = new_project(dir.path());
    p.add_user_field(
        "settlement",
        "climate",
        "Climate",
        FieldType::Choice(vec!["dry".into(), "wet".into()]),
    )
    .unwrap();
    p.add_user_field("settlement", "walled", "Walled", FieldType::YesNo)
        .unwrap();
    p.add_user_field("settlement", "notes", "Notes", FieldType::Markdown)
        .unwrap();
    let town = create(
        &mut p,
        "settlement",
        "Brindle",
        json!({"population": 1200, "climate": "dry", "walled": true, "notes": "# Hi"}),
    )
    .unwrap();
    let nation = create(
        &mut p,
        "nation",
        "Oskar",
        json!({"capital": Value::from(town)}),
    )
    .unwrap();
    assert_eq!(
        p.entity(nation).unwrap().unwrap().fields["capital"],
        Value::from(town)
    );

    let bad = [
        ("settlement", json!({"population": "many"})),
        ("settlement", json!({"climate": "cold"})),
        ("settlement", json!({"walled": 1})),
        ("settlement", json!({"no_such_field": 1})),
        ("nation", json!({"capital": 9999})),
        ("nation", json!({"population": 5})),
    ];
    for (kind, f) in bad {
        let r = create(&mut p, kind, "X", f.clone());
        assert!(
            matches!(r, Err(Error::InvalidField { .. })),
            "{kind} {f} gave {r:?}"
        );
    }
    let r = edit(
        &mut p,
        EntityEdit::SetField {
            id: town,
            key: "population".into(),
            value: json!(true),
        },
    );
    assert!(matches!(r, Err(Error::InvalidField { .. })));
    assert!(matches!(
        create(&mut p, "dragon", "X", json!({})),
        Err(Error::UnknownEntityKind(_))
    ));
    assert_eq!(p.entities().unwrap().len(), 2);
}

#[test]
fn entity_edits_undo_and_redo_on_the_active_map() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = new_project(dir.path());
    let map = p.maps()[0].id;
    let other = p
        .add_root_map(MapSettings::default_for(MapKind::World))
        .unwrap();
    let a = create(&mut p, "settlement", "Ash", json!({"population": 10})).unwrap();
    let b = create(&mut p, "region", "Vale", json!({})).unwrap();
    let rel = Relation {
        from: a,
        to: b,
        kind: "part_of".into(),
    };
    edit(&mut p, EntityEdit::Relate(rel.clone())).unwrap();
    edit(
        &mut p,
        EntityEdit::Rename {
            id: a,
            name: "Ashford".into(),
        },
    )
    .unwrap();
    edit(
        &mut p,
        EntityEdit::SetField {
            id: a,
            key: "population".into(),
            value: Value::Null,
        },
    )
    .unwrap();
    assert!(p.entity(a).unwrap().unwrap().fields.is_empty());

    assert!(!p.undo(other).unwrap(), "another map's history is empty");
    assert!(p.undo(map).unwrap());
    assert_eq!(
        p.entity(a).unwrap().unwrap().fields["population"],
        json!(10)
    );
    assert!(p.undo(map).unwrap());
    assert_eq!(p.entity(a).unwrap().unwrap().name, "Ash");
    assert!(p.redo(map).unwrap());
    assert_eq!(p.entity(a).unwrap().unwrap().name, "Ashford");

    edit(&mut p, EntityEdit::Delete(a)).unwrap();
    assert!(p.entity(a).unwrap().is_none());
    assert!(p.relations().unwrap().is_empty());
    assert!(p.undo(map).unwrap());
    assert_eq!(p.relations().unwrap(), vec![rel.clone()]);
    assert_eq!(p.entity(a).unwrap().unwrap().name, "Ashford");

    // Undo the rename, the relation, then the creation of b.
    for _ in 0..3 {
        assert!(p.undo(map).unwrap());
    }
    assert!(p.relations().unwrap().is_empty());
    assert!(p.entity(b).unwrap().is_none());
    let c = create(&mut p, "lake", "Mere", json!({})).unwrap();
    assert_ne!(c, b, "ids are never reused");
}

#[test]
fn entities_fields_and_relations_survive_save_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = new_project(dir.path());
    p.add_user_field("river", "length_km", "Length", FieldType::Number)
        .unwrap();
    let r = create(&mut p, "river", "Sill", json!({"length_km": 42.5})).unwrap();
    let s = create(&mut p, "sea", "Grey Sea", json!({})).unwrap();
    let rel = Relation {
        from: r,
        to: s,
        kind: "flows_into".into(),
    };
    edit(&mut p, EntityEdit::Relate(rel.clone())).unwrap();
    let file = dir.path().join("world.mmc");
    p.save(&file);
    p.close().unwrap();

    let mut p = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    let river = p.entity(r).unwrap().unwrap();
    assert_eq!(river.name, "Sill");
    assert_eq!(river.fields["length_km"], json!(42.5));
    assert_eq!(p.relations().unwrap(), vec![rel]);
    assert!(
        p.field_defs("river")
            .unwrap()
            .iter()
            .any(|d| d.key == "length_km" && !d.builtin)
    );
    let n = create(&mut p, "island", "Holm", json!({})).unwrap();
    assert!(n != r && n != s);
}
