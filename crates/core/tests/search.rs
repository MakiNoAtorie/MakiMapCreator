use mmc_core::{
    Command, Entity, EntityEdit, EntityId, MapId, MapKind, MapSettings, PageEdit, Project,
};
use serde_json::json;

fn new_project(dir: &std::path::Path) -> (Project, MapId) {
    let p = mmc_fixtures::create(
        &dir.join("wc.sqlite"),
        MapSettings::default_for(MapKind::Region),
    )
    .unwrap();
    let m = p.maps()[0].id;
    (p, m)
}

fn create(p: &mut Project, m: MapId, kind: &str, name: &str) -> EntityId {
    let id = p.next_entity_id().unwrap();
    let e = Entity {
        id,
        kind: kind.into(),
        name: name.into(),
        fields: Default::default(),
    };
    p.apply(m, Command::Entity(EntityEdit::Create(e))).unwrap();
    id
}

fn page(p: &mut Project, m: MapId, entity: EntityId, text: &str) {
    let edit = PageEdit {
        entity,
        text: text.into(),
    };
    p.apply(m, Command::Page(edit)).unwrap();
}

fn names(p: &Project, q: &str) -> Vec<EntityId> {
    p.find_names(q, 10).into_iter().map(|h| h.id).collect()
}

fn text(p: &Project, q: &str) -> Vec<EntityId> {
    p.search_text(q, 10)
        .unwrap()
        .wait()
        .unwrap()
        .into_iter()
        .map(|h| h.entity)
        .collect()
}

#[test]
fn name_lookup_ranks_prefix_then_word_then_fuzzy() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, m) = new_project(dir.path());
    let a = create(&mut p, m, "settlement", "Rivertown");
    let b = create(&mut p, m, "river", "Silver River");
    let c = create(&mut p, m, "region", "Ironhold");
    assert_eq!(names(&p, "river"), [a, b]);
    assert_eq!(names(&p, "RIV"), [a, b]);
    assert_eq!(names(&p, "ironhlod"), [c]);
    assert!(names(&p, "zzz").is_empty());
    assert!(names(&p, "").is_empty());
}

#[test]
fn name_index_follows_edits_undo_redo_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, m) = new_project(dir.path());
    let a = create(&mut p, m, "region", "Alpha");
    assert_eq!(names(&p, "alp"), [a]);
    p.apply(
        m,
        Command::Entity(EntityEdit::Rename {
            id: a,
            name: "Beta".into(),
        }),
    )
    .unwrap();
    assert!(names(&p, "alp").is_empty());
    assert_eq!(names(&p, "bet"), [a]);
    p.undo(m).unwrap();
    assert_eq!(names(&p, "alp"), [a]);
    p.redo(m).unwrap();
    assert_eq!(names(&p, "bet"), [a]);
    p.apply(m, Command::Entity(EntityEdit::Delete(a))).unwrap();
    assert!(names(&p, "bet").is_empty());
    p.undo(m).unwrap();
    assert_eq!(names(&p, "bet"), [a]);

    let file = dir.path().join("p.mmc");
    p.save(&file);
    p.wait_for_save().unwrap();
    let q = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert_eq!(names(&q, "bet"), [a]);
}

#[test]
fn full_text_finds_names_fields_and_pages_and_follows_edits() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, m) = new_project(dir.path());
    let town = create(&mut p, m, "settlement", "Rivertown");
    let nation = create(&mut p, m, "nation", "Orland");
    p.apply(
        m,
        Command::Entity(EntityEdit::SetField {
            id: nation,
            key: "colour".into(),
            value: json!("crimson banners"),
        }),
    )
    .unwrap();
    page(&mut p, m, town, "## History\n\nFounded by dragonslayers.");
    assert_eq!(text(&p, "dragonslayers"), [town]);
    assert_eq!(text(&p, "dragon"), [town]);
    assert_eq!(text(&p, "crimson"), [nation]);
    assert_eq!(text(&p, "rivertown"), [town]);
    assert!(text(&p, "\"* OR NEAR(").is_empty());

    page(&mut p, m, town, "Nothing here.");
    assert!(text(&p, "dragonslayers").is_empty());
    p.undo(m).unwrap();
    assert_eq!(text(&p, "dragonslayers"), [town]);
    p.apply(m, Command::Entity(EntityEdit::Delete(town)))
        .unwrap();
    assert!(text(&p, "dragonslayers").is_empty());
    p.undo(m).unwrap();
    assert_eq!(text(&p, "dragonslayers"), [town]);
}

#[test]
fn full_text_runs_on_a_worker_the_caller_can_poll() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, m) = new_project(dir.path());
    let a = create(&mut p, m, "region", "Alpha");
    page(&mut p, m, a, "needle in text");
    let mut h = p.search_text("needle", 5).unwrap();
    let hits = loop {
        if let Some(r) = h.poll() {
            break r.unwrap();
        }
        std::thread::yield_now();
    };
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.as_deref().unwrap().contains("needle"));
    assert!(h.poll().is_none());
}
