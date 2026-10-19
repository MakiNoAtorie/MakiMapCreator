use mmc_core::{
    Command, Document, Entity, EntityEdit, EntityId, Error, MapKind, MapSettings, PageEdit,
    Project, link_markup,
};

fn new_project(dir: &std::path::Path) -> Project {
    Project::create(
        &dir.join("wc.sqlite"),
        MapSettings::default_for(MapKind::Region),
    )
    .unwrap()
}

fn create(p: &mut Project, kind: &str, name: &str) -> EntityId {
    let id = p.next_entity_id().unwrap();
    let e = Entity {
        id,
        kind: kind.into(),
        name: name.into(),
        fields: Default::default(),
    };
    p.apply(p.maps()[0].id, Command::Entity(EntityEdit::Create(e)))
        .unwrap();
    id
}

fn write(p: &mut Project, entity: EntityId, text: &str) {
    let edit = PageEdit {
        entity,
        text: text.into(),
    };
    p.apply(p.maps()[0].id, Command::Page(edit)).unwrap();
}

fn names(p: &Project, of: EntityId) -> Vec<(String, Option<String>)> {
    p.links(of)
        .unwrap()
        .into_iter()
        .map(|l| (l.label, l.entity.map(|e| e.name)))
        .collect()
}

#[test]
fn a_new_page_starts_with_its_kind_template() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = new_project(dir.path());
    let town = create(&mut p, "settlement", "Rivertown");
    let river = create(&mut p, "river", "Silver");
    let town_page = p.page(town).unwrap();
    assert!(town_page.contains("## History"), "{town_page}");
    assert!(town_page.contains("## People"), "{town_page}");
    assert!(p.page(river).unwrap().contains("## Course"));
}

#[test]
fn page_of_an_unknown_entity_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = new_project(dir.path());
    let a = create(&mut p, "region", "A");
    p.apply(p.maps()[0].id, Command::Entity(EntityEdit::Delete(a)))
        .unwrap();
    assert!(matches!(p.page(a), Err(Error::UnknownEntity(id)) if id == a));
    let edit = PageEdit {
        entity: a,
        text: "x".into(),
    };
    assert!(p.apply(p.maps()[0].id, Command::Page(edit)).is_err());
}

#[test]
fn renaming_the_target_keeps_the_link_resolving() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = new_project(dir.path());
    let region = create(&mut p, "region", "Vale");
    let town = create(&mut p, "settlement", "Rivertown");
    let text = format!("Capital: {}.", link_markup(town, "Rivertown"));
    write(&mut p, region, &text);
    let map = p.maps()[0].id;
    p.apply(
        map,
        Command::Entity(EntityEdit::Rename {
            id: town,
            name: "Riverton".into(),
        }),
    )
    .unwrap();
    assert_eq!(p.page(region).unwrap(), text);
    assert_eq!(
        names(&p, region),
        [("Rivertown".to_owned(), Some("Riverton".to_owned()))]
    );
    assert_eq!(p.links(region).unwrap()[0].target, town);
}

#[test]
fn a_link_to_a_deleted_entity_is_missing_and_its_text_stays() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = new_project(dir.path());
    let region = create(&mut p, "region", "Vale");
    let town = create(&mut p, "settlement", "Rivertown");
    let text = format!("See {}.", link_markup(town, "Rivertown"));
    write(&mut p, region, &text);
    let map = p.maps()[0].id;
    p.apply(map, Command::Entity(EntityEdit::Delete(town)))
        .unwrap();
    assert_eq!(p.page(region).unwrap(), text);
    assert_eq!(names(&p, region), [("Rivertown".to_owned(), None)]);
    p.undo(map).unwrap();
    assert_eq!(
        names(&p, region),
        [("Rivertown".to_owned(), Some("Rivertown".to_owned()))]
    );
}

#[test]
fn backlinks_list_every_page_that_links_to_an_entity() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = new_project(dir.path());
    let a = create(&mut p, "region", "A");
    let b = create(&mut p, "settlement", "B");
    let c = create(&mut p, "landmark", "C");
    let d = create(&mut p, "landmark", "D");
    write(&mut p, a, &format!("{0} and {0}", link_markup(b, "B")));
    write(
        &mut p,
        c,
        &format!("{} near {}", link_markup(b, "B"), link_markup(a, "A")),
    );
    write(&mut p, d, "no links");
    assert_eq!(p.backlinks(b).unwrap(), [a, c]);
    assert_eq!(p.backlinks(a).unwrap(), [c]);
    assert!(p.backlinks(d).unwrap().is_empty());

    write(&mut p, a, "link removed");
    assert_eq!(p.backlinks(b).unwrap(), [c]);
    let map = p.maps()[0].id;
    p.undo(map).unwrap();
    assert_eq!(p.backlinks(b).unwrap(), [a, c]);

    // The page of a deleted entity no longer counts.
    p.apply(map, Command::Entity(EntityEdit::Delete(c)))
        .unwrap();
    assert_eq!(p.backlinks(b).unwrap(), [a]);
}

#[test]
fn a_group_of_page_edits_is_one_command() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = new_project(dir.path());
    let a = create(&mut p, "region", "A");
    let b = create(&mut p, "settlement", "B");
    let template = p.page(a).unwrap();

    let mut doc = Document::new(template.as_str().into());
    doc.select(doc.text.len_chars(), false);
    doc.insert("Founded long ago.");
    doc.enter();
    doc.insert(&link_markup(b, "B"));
    doc.undo();
    doc.insert(&link_markup(b, "the town"));
    let edited = doc.text.to_string();
    write(&mut p, a, &edited);

    let map = p.maps()[0].id;
    assert!(p.undo(map).unwrap());
    assert_eq!(p.page(a).unwrap(), template);
    assert!(p.backlinks(b).unwrap().is_empty());
    assert!(p.redo(map).unwrap());
    assert_eq!(p.page(a).unwrap(), edited);
    assert_eq!(p.backlinks(b).unwrap(), [a]);
    // The undo before the group is the entity creation.
    p.undo(map).unwrap();
    p.undo(map).unwrap();
    assert!(p.entity(b).unwrap().is_none());
}

#[test]
fn pages_and_backlinks_survive_save_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = new_project(dir.path());
    let a = create(&mut p, "region", "A");
    let b = create(&mut p, "settlement", "B");
    let text = format!("# A\n\nHome of {}.\n", link_markup(b, "B"));
    write(&mut p, a, &text);
    let file = dir.path().join("world.mmc");
    p.save(&file);
    p.close().unwrap();

    let p = Project::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert_eq!(p.page(a).unwrap(), text);
    assert_eq!(p.backlinks(b).unwrap(), [a]);
}
