use mmc_core::{
    APP_VERSION, Command, Error, Generation, MapId, MapKind, MapSettings, NewEntity, Project,
    RootPoint,
};

fn world() -> MapSettings {
    MapSettings {
        kind: MapKind::World,
        width: 512,
        height: 512,
        metres_per_pixel: 1000.0,
    }
}

fn square(x: f64) -> Vec<RootPoint> {
    [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
        .iter()
        .map(|(a, b)| RootPoint {
            x: x + a * 1000.0,
            y: b * 1000.0,
        })
        .collect()
}

fn generation(kind: &str) -> Command {
    Command::Generate(Generation {
        generator: "regions".into(),
        seed: 42,
        entities: vec![
            NewEntity {
                kind: kind.into(),
                name: "North".into(),
                outline: square(0.0),
            },
            NewEntity {
                kind: kind.into(),
                name: "South".into(),
                outline: square(5000.0),
            },
        ],
        rasters: vec![],
    })
}

/// (name, outline) of each region, ordered by entity id.
fn regions(p: &Project, map: MapId) -> Vec<(String, Vec<RootPoint>)> {
    p.outlines(map)
        .unwrap()
        .into_iter()
        .map(|(id, o)| (p.entity(id).unwrap().unwrap().name, o))
        .collect()
}

#[test]
fn generation_creates_entities_with_outlines_and_records_version_and_seed() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Project::create(&dir.path().join("wc.sqlite"), world()).unwrap();
    let map = p.maps()[0].id;
    p.apply(map, generation("region")).unwrap();

    let expected = vec![
        ("North".to_string(), square(0.0)),
        ("South".to_string(), square(5000.0)),
    ];
    assert_eq!(regions(&p, map), expected);
    assert!(p.entities().unwrap().iter().all(|e| e.kind == "region"));
    let g = p.generations(map).unwrap();
    assert_eq!(g.len(), 1);
    assert_eq!(
        (
            g[0].generator.as_str(),
            g[0].seed,
            g[0].app_version.as_str()
        ),
        ("regions", 42, APP_VERSION)
    );

    assert_eq!(p.find_names("nort", 5)[0].name, "North");
    let hits = p.search_text("south", 5).unwrap().wait().unwrap();
    assert_eq!(hits.len(), 1);

    p.undo(map).unwrap();
    assert!(p.find_names("nort", 5).is_empty());
    assert!(p.entities().unwrap().is_empty());
    assert!(p.outlines(map).unwrap().is_empty());
    assert!(p.generations(map).unwrap().is_empty());

    p.redo(map).unwrap();
    assert_eq!(regions(&p, map), expected);

    let file = dir.path().join("w.mmc");
    p.save(&file).unwrap();
    p.close().unwrap();
    let p = Project::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert_eq!(regions(&p, map), expected);
    assert_eq!(p.generations(map).unwrap().len(), 1);
}

#[test]
fn generation_refuses_an_unknown_entity_kind() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Project::create(&dir.path().join("wc.sqlite"), world()).unwrap();
    let map = p.maps()[0].id;
    let err = p.apply(map, generation("galaxy")).unwrap_err();
    assert!(matches!(err, Error::UnknownEntityKind(_)), "{err}");
    assert!(p.entities().unwrap().is_empty());
    assert!(p.generations(map).unwrap().is_empty());
}
