use mmc_core::{
    AnchorEdit, Anchors, Command, DerivedFeature, Entity, EntityEdit, EntityId, FeatureId,
    FeatureKind, MapId, MapKind, MapSettings, PageEdit, Project, RootPoint,
};

fn pt(x: f64, y: f64) -> RootPoint {
    RootPoint { x, y }
}

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

fn anchor(p: &mut Project, m: MapId, entity: EntityId, anchors: Anchors) {
    p.apply(m, Command::Anchor(AnchorEdit::Set { entity, anchors }))
        .unwrap();
}

/// A river running east along `y`, mouth at x = 10_000.
fn river(id: u64, y: f64) -> DerivedFeature {
    DerivedFeature {
        id: FeatureId(id),
        kind: FeatureKind::River,
        geometry: vec![pt(0.0, y), pt(5_000.0, y), pt(10_000.0, y)],
    }
}

fn lake(id: u64, cx: f64, cy: f64) -> DerivedFeature {
    let r = 1_000.0;
    DerivedFeature {
        id: FeatureId(id),
        kind: FeatureKind::Lake,
        geometry: vec![
            pt(cx - r, cy - r),
            pt(cx + r, cy - r),
            pt(cx + r, cy + r),
            pt(cx - r, cy + r),
        ],
    }
}

fn silver_anchors(y: f64) -> Anchors {
    Anchors::River {
        mouth: pt(10_000.0, y),
        picked: pt(4_000.0, y),
    }
}

#[test]
fn rebuild_attaches_the_nearest_feature_within_tolerance() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, m) = new_project(dir.path());
    let silver = create(&mut p, m, "river", "Silver");
    anchor(&mut p, m, silver, silver_anchors(1_000.0));
    assert_eq!(p.attached_feature(silver).unwrap(), None);
    // The river moved 300 m; a second river lies 5 km away.
    p.set_derived_features(vec![river(1, 5_000.0), river(2, 1_300.0)]);
    p.rebuild_derived(m).unwrap();
    p.wait_for_jobs().unwrap();
    assert_eq!(p.attached_feature(silver).unwrap(), Some(FeatureId(2)));
    assert!(p.unplaced_entities().unwrap().is_empty());
}

#[test]
fn a_lake_matches_by_its_interior_point() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, m) = new_project(dir.path());
    let mere = create(&mut p, m, "lake", "Mere");
    anchor(
        &mut p,
        m,
        mere,
        Anchors::Lake {
            inside: pt(100.0, 50.0),
        },
    );
    p.set_derived_features(vec![lake(7, 0.0, 0.0), lake(8, 9_000.0, 0.0)]);
    p.match_anchors().unwrap();
    assert_eq!(p.attached_feature(mere).unwrap(), Some(FeatureId(7)));
}

#[test]
fn no_match_leaves_the_entity_unplaced_with_its_page() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, m) = new_project(dir.path());
    let silver = create(&mut p, m, "river", "Silver");
    p.apply(
        m,
        Command::Page(PageEdit {
            entity: silver,
            text: "The Silver runs east.".into(),
        }),
    )
    .unwrap();
    anchor(&mut p, m, silver, silver_anchors(1_000.0));
    p.set_derived_features(vec![river(1, 1_200.0)]);
    p.match_anchors().unwrap();
    assert_eq!(p.attached_feature(silver).unwrap(), Some(FeatureId(1)));

    // The river is now 2 km away: beyond the 500 m default tolerance.
    p.set_derived_features(vec![river(1, 3_000.0)]);
    p.match_anchors().unwrap();
    assert_eq!(p.unplaced_entities().unwrap(), vec![silver]);
    assert_eq!(p.attached_feature(silver).unwrap(), None);
    assert_eq!(p.page(silver).unwrap(), "The Silver runs east.");
    assert!(p.entity(silver).unwrap().is_some());
}

#[test]
fn tolerance_is_configurable() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, m) = new_project(dir.path());
    let silver = create(&mut p, m, "river", "Silver");
    anchor(&mut p, m, silver, silver_anchors(1_000.0));
    p.set_derived_features(vec![river(1, 3_000.0)]);
    p.set_anchor_tolerance(2_500.0);
    p.match_anchors().unwrap();
    assert_eq!(p.attached_feature(silver).unwrap(), Some(FeatureId(1)));
}

#[test]
fn reattach_places_an_unplaced_entity_and_undoes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, m) = new_project(dir.path());
    let silver = create(&mut p, m, "river", "Silver");
    anchor(&mut p, m, silver, silver_anchors(1_000.0));
    p.set_derived_features(vec![river(1, 3_000.0)]);
    p.match_anchors().unwrap();
    assert_eq!(p.unplaced_entities().unwrap(), vec![silver]);

    p.apply(
        m,
        Command::Anchor(AnchorEdit::Reattach {
            entity: silver,
            feature: FeatureId(1),
        }),
    )
    .unwrap();
    assert!(p.unplaced_entities().unwrap().is_empty());
    assert_eq!(p.attached_feature(silver).unwrap(), Some(FeatureId(1)));
    // The anchors follow the feature, so the next rebuild keeps the match.
    p.match_anchors().unwrap();
    assert_eq!(p.attached_feature(silver).unwrap(), Some(FeatureId(1)));

    assert!(p.undo(m).unwrap());
    assert_eq!(p.unplaced_entities().unwrap(), vec![silver]);
    assert_eq!(p.anchors(silver).unwrap(), Some(silver_anchors(1_000.0)));
}

#[test]
fn reattach_to_a_missing_or_wrong_kind_feature_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, m) = new_project(dir.path());
    let silver = create(&mut p, m, "river", "Silver");
    anchor(&mut p, m, silver, silver_anchors(0.0));
    p.set_derived_features(vec![lake(3, 0.0, 0.0)]);
    for f in [FeatureId(99), FeatureId(3)] {
        let cmd = Command::Anchor(AnchorEdit::Reattach {
            entity: silver,
            feature: f,
        });
        assert!(p.apply(m, cmd).is_err());
    }
}

#[test]
fn anchors_must_fit_the_entity_kind() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, m) = new_project(dir.path());
    let town = create(&mut p, m, "settlement", "Town");
    let mere = create(&mut p, m, "lake", "Mere");
    let bad = |entity, anchors| Command::Anchor(AnchorEdit::Set { entity, anchors });
    assert!(p.apply(m, bad(town, silver_anchors(0.0))).is_err());
    assert!(p.apply(m, bad(mere, silver_anchors(0.0))).is_err());
}

#[test]
fn anchors_and_unplaced_state_survive_save_and_open() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, m) = new_project(dir.path());
    let silver = create(&mut p, m, "river", "Silver");
    anchor(&mut p, m, silver, silver_anchors(1_000.0));
    p.set_derived_features(vec![]);
    p.match_anchors().unwrap();
    let file = dir.path().join("a.mmc");
    p.save(&file);
    p.close().unwrap();
    let q = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert_eq!(q.anchors(silver).unwrap(), Some(silver_anchors(1_000.0)));
    assert_eq!(q.unplaced_entities().unwrap(), vec![silver]);
}
