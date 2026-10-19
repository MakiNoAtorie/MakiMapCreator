use mmc_core::{
    BrushStroke, Command, Layer, MapId, MapSettings, Project, RootPoint, StrokePoint, TILE_SIZE,
    UNDO_LIMIT,
};
use mmc_kind_region::REGION;

fn settings() -> MapSettings {
    MapSettings {
        kind: REGION,
        width: 1024,
        height: 768,
        metres_per_pixel: 10.0,
    }
}

fn stroke(x: f64, y: f64, strength: f32) -> Command {
    Command::Brush(BrushStroke {
        layer: Layer::HEIGHT,
        points: vec![StrokePoint {
            at: RootPoint { x, y },
            pressure: 1.0,
            radius: 50.0,
        }],
        strength,
        falloff: 1.0,
    })
}

fn height(p: &Project, map: MapId, x: f64, y: f64) -> f32 {
    p.layer_value(map, Layer::HEIGHT, RootPoint { x, y })
        .unwrap()
        .unwrap()
}

#[test]
fn new_map_is_at_sea_level_and_outside_points_have_no_value() {
    let dir = tempfile::tempdir().unwrap();
    let p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    assert_eq!(height(&p, map, 5000.0, 5000.0), 0.0);
    let outside = p
        .layer_value(
            map,
            Layer::HEIGHT,
            RootPoint {
                x: 5000.0,
                y: 7690.0,
            },
        )
        .unwrap();
    assert_eq!(outside, None);
    assert!(p.edited_tiles(map, Layer::HEIGHT).unwrap().is_empty());
}

#[test]
fn brush_raises_height_inside_radius_only_and_scales_with_pressure() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    p.apply(map, stroke(2005.0, 2005.0, 10.0)).unwrap();
    let centre = height(&p, map, 2005.0, 2005.0);
    assert!(centre > 9.0 && centre <= 10.0, "centre {centre}");
    let mid = height(&p, map, 2030.0, 2005.0);
    assert!(mid > 0.0 && mid < centre, "mid {mid}");
    assert_eq!(height(&p, map, 2065.0, 2005.0), 0.0);

    let half = Command::Brush(BrushStroke {
        layer: Layer::HEIGHT,
        points: vec![StrokePoint {
            at: RootPoint {
                x: 6005.0,
                y: 2005.0,
            },
            pressure: 0.5,
            radius: 50.0,
        }],
        strength: 10.0,
        falloff: 1.0,
    });
    p.apply(map, half).unwrap();
    assert_eq!(height(&p, map, 6005.0, 2005.0), centre / 2.0);
}

#[test]
fn stroke_across_a_tile_corner_edits_four_tiles() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    let edge = TILE_SIZE as f64 * 10.0;
    p.apply(map, stroke(edge, edge, -5.0)).unwrap();
    assert!(height(&p, map, edge - 5.0, edge - 5.0) < 0.0);
    assert!(height(&p, map, edge + 5.0, edge + 5.0) < 0.0);
    assert_eq!(
        p.edited_tiles(map, Layer::HEIGHT).unwrap(),
        vec![(0, 0), (0, 1), (1, 0), (1, 1)]
    );
}

#[test]
fn undo_and_redo_restore_heights_and_edited_flags() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    p.apply(map, stroke(105.0, 105.0, 10.0)).unwrap();
    p.apply(map, stroke(105.0, 105.0, 10.0)).unwrap();
    let twice = height(&p, map, 105.0, 105.0);

    assert!(p.undo(map).unwrap());
    let once = height(&p, map, 105.0, 105.0);
    assert!((once - twice / 2.0).abs() < 1e-4);
    assert!(p.undo(map).unwrap());
    assert_eq!(height(&p, map, 105.0, 105.0), 0.0);
    assert!(p.edited_tiles(map, Layer::HEIGHT).unwrap().is_empty());
    assert!(!p.undo(map).unwrap());

    assert!(p.redo(map).unwrap());
    assert_eq!(height(&p, map, 105.0, 105.0), once);
    assert_eq!(p.edited_tiles(map, Layer::HEIGHT).unwrap(), vec![(0, 0)]);
    assert!(p.redo(map).unwrap());
    assert_eq!(height(&p, map, 105.0, 105.0), twice);
    assert!(!p.redo(map).unwrap());
}

#[test]
fn a_new_command_clears_redo() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    p.apply(map, stroke(105.0, 105.0, 10.0)).unwrap();
    p.undo(map).unwrap();
    p.apply(map, stroke(3005.0, 105.0, 10.0)).unwrap();
    assert!(!p.redo(map).unwrap());
    assert_eq!(height(&p, map, 105.0, 105.0), 0.0);
}

#[test]
fn undo_on_one_map_never_changes_another() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let a = p.maps()[0].id;
    let b = p.add_root_map(settings()).unwrap();
    p.apply(a, stroke(105.0, 105.0, 10.0)).unwrap();
    p.apply(b, stroke(105.0, 105.0, 7.0)).unwrap();
    let hb = height(&p, b, 105.0, 105.0);

    assert!(p.undo(a).unwrap());
    assert_eq!(height(&p, a, 105.0, 105.0), 0.0);
    assert_eq!(height(&p, b, 105.0, 105.0), hb);
    assert!(!p.undo(a).unwrap());
    assert!(p.undo(b).unwrap());
    assert_eq!(height(&p, b, 105.0, 105.0), 0.0);
}

#[test]
fn undo_history_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    for _ in 0..UNDO_LIMIT + 5 {
        p.apply(map, stroke(105.0, 105.0, 1.0)).unwrap();
    }
    let mut undone = 0;
    while p.undo(map).unwrap() {
        undone += 1;
    }
    assert_eq!(undone, UNDO_LIMIT);
    assert!(height(&p, map, 105.0, 105.0) > 4.0);
}

#[test]
fn heights_and_edited_flags_survive_save_and_reload_but_undo_does_not() {
    let app_data = tempfile::tempdir().unwrap();
    let docs = tempfile::tempdir().unwrap();
    let file = docs.path().join("w.mmc");
    let mut p = mmc_fixtures::create(&app_data.path().join("a.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    p.apply(map, stroke(5005.0, 3005.0, 12.0)).unwrap();
    let h = height(&p, map, 5005.0, 3005.0);
    let edited = p.edited_tiles(map, Layer::HEIGHT).unwrap();
    assert_eq!(edited, vec![(1, 1)]);
    p.save(&file);
    p.close().unwrap();

    let mut p = mmc_fixtures::open(&file, &app_data.path().join("b.sqlite")).unwrap();
    assert_eq!(height(&p, map, 5005.0, 3005.0), h);
    assert_eq!(height(&p, map, 105.0, 105.0), 0.0);
    assert_eq!(p.edited_tiles(map, Layer::HEIGHT).unwrap(), edited);
    assert!(!p.undo(map).unwrap());
    p.close().unwrap();
}

#[test]
fn each_point_has_its_own_radius_and_falloff_shapes_the_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    let point = |x: f64, radius: f64| StrokePoint {
        at: RootPoint { x, y: 2005.0 },
        pressure: 1.0,
        radius,
    };
    let brush = |falloff: f32, points| {
        Command::Brush(BrushStroke {
            layer: Layer::HEIGHT,
            points,
            strength: 10.0,
            falloff,
        })
    };
    // One command: a small stamp at x = 1005 and a large one at x = 4005.
    p.apply(
        map,
        brush(1.0, vec![point(1005.0, 30.0), point(4005.0, 300.0)]),
    )
    .unwrap();
    assert_eq!(height(&p, map, 1105.0, 2005.0), 0.0);
    assert!(height(&p, map, 4105.0, 2005.0) > 0.0);
    assert!(p.undo(map).unwrap());
    assert!(!p.undo(map).unwrap(), "one stroke is one undo step");

    // Hard (0) keeps full strength out to the rim; soft (1) fades.
    p.apply(map, brush(0.0, vec![point(1005.0, 100.0)]))
        .unwrap();
    p.apply(map, brush(1.0, vec![point(4005.0, 100.0)]))
        .unwrap();
    assert_eq!(height(&p, map, 1085.0, 2005.0), 10.0);
    let soft = height(&p, map, 4085.0, 2005.0);
    assert!(soft > 0.0 && soft < 2.0, "{soft}");
}
