use mmc_core::{
    BrushStroke, Command, Error, Layer, MapId, MapSettings, Project, RootPoint, StrokePoint,
    TILE_SIZE,
};
use mmc_kind_region::{REGION, SLOPE};

fn settings() -> MapSettings {
    MapSettings {
        kind: REGION,
        width: 1024,
        height: 768,
        metres_per_pixel: 10.0,
    }
}

fn stroke(layer: Layer, x: f64, y: f64, strength: f32) -> Command {
    Command::Brush(BrushStroke {
        layer,
        points: vec![StrokePoint {
            at: RootPoint { x, y },
            pressure: 1.0,
            radius: 300.0,
        }],
        strength,
        falloff: 1.0,
        target: None,
        ..Default::default()
    })
}

fn slope(p: &Project, map: MapId, x: f64, y: f64) -> f32 {
    p.layer_value(map, SLOPE, RootPoint { x, y })
        .unwrap()
        .unwrap()
}

fn all_tiles(p: &Project, map: MapId, layer: Layer) -> Vec<Vec<f32>> {
    let s = settings();
    let mut out = vec![];
    for ty in 0..s.height.div_ceil(TILE_SIZE) {
        for tx in 0..s.width.div_ceil(TILE_SIZE) {
            out.push(p.layer_tile(map, layer, tx, ty).unwrap());
        }
    }
    out
}

/// Strokes across tile borders, so slope needs neighbour tiles.
fn strokes() -> Vec<Command> {
    vec![
        stroke(Layer::HEIGHT, 2560.0, 2560.0, 40.0),
        stroke(Layer::HEIGHT, 5000.0, 2400.0, -25.0),
        stroke(Layer::HEIGHT, 7700.0, 5100.0, 60.0),
        stroke(Layer::HEIGHT, 10230.0, 7670.0, 30.0),
    ]
}

#[test]
fn layers_are_authored_or_derived_and_derived_layers_refuse_commands() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    assert!(!p.is_derived(map, Layer::HEIGHT).unwrap());
    assert!(p.is_derived(map, SLOPE).unwrap());
    let err = p
        .apply(map, stroke(SLOPE, 2000.0, 2000.0, 1.0))
        .unwrap_err();
    assert!(matches!(err, Error::DerivedLayer(SLOPE)), "{err}");
    assert!(!p.undo(map).unwrap());
    assert_eq!(slope(&p, map, 2000.0, 2000.0), 0.0);
}

#[test]
fn slope_rebuilds_after_a_stroke_and_undo_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    p.apply(map, stroke(Layer::HEIGHT, 2005.0, 2005.0, 50.0))
        .unwrap();
    // Double-buffered: a read sees the old or the new complete tile, never a mix.
    let during = p.layer_tile(map, SLOPE, 0, 0).unwrap();
    p.wait_for_jobs().unwrap();
    let after = p.layer_tile(map, SLOPE, 0, 0).unwrap();
    assert!(during.iter().all(|v| *v == 0.0) || during == after);

    let flank = slope(&p, map, 2105.0, 2005.0);
    assert!(flank > 0.01, "flank {flank}");
    assert!(slope(&p, map, 2005.0, 2005.0) < 1e-6);
    assert_eq!(slope(&p, map, 2405.0, 2005.0), 0.0);
    assert!(p.edited_tiles(map, SLOPE).unwrap().is_empty());

    p.undo(map).unwrap();
    p.wait_for_jobs().unwrap();
    assert_eq!(slope(&p, map, 2105.0, 2005.0), 0.0);
    p.redo(map).unwrap();
    p.wait_for_jobs().unwrap();
    assert_eq!(slope(&p, map, 2105.0, 2005.0), flank);
}

#[test]
fn derived_tiles_are_saved_so_open_does_not_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("p.mmc");
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    p.apply(map, stroke(Layer::HEIGHT, 2005.0, 2005.0, 50.0))
        .unwrap();
    p.wait_for_jobs().unwrap();
    let flank = slope(&p, map, 2105.0, 2005.0);
    p.save(&file);
    p.close().unwrap();

    let mut q = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert_eq!(slope(&q, map, 2105.0, 2005.0), flank);

    // After open, a rebuild that flattens the terrain must still remove the saved slope tiles.
    q.apply(map, stroke(Layer::HEIGHT, 2005.0, 2005.0, -50.0))
        .unwrap();
    q.wait_for_jobs().unwrap();
    assert!(
        all_tiles(&q, map, SLOPE)
            .iter()
            .flatten()
            .all(|v| *v == 0.0)
    );
    q.close().unwrap();
}

#[test]
fn same_edits_give_the_same_derived_tiles_with_1_and_4_threads() {
    let dir = tempfile::tempdir().unwrap();
    let mut results = vec![];
    for (i, threads) in [1, 4].into_iter().enumerate() {
        let mut p =
            mmc_fixtures::create(&dir.path().join(format!("wc{i}.sqlite")), settings()).unwrap();
        p.set_worker_threads(threads);
        let map = p.maps()[0].id;
        for c in strokes() {
            p.apply(map, c).unwrap();
        }
        p.wait_for_jobs().unwrap();
        results.push(all_tiles(&p, map, SLOPE));
    }
    assert!(results[0].iter().flatten().any(|v| *v > 0.0));
    assert!(results[0] == results[1]);
}

#[test]
fn a_newer_request_replaces_an_older_one_and_full_rebuild_matches() {
    let dir = tempfile::tempdir().unwrap();
    let mut quick = mmc_fixtures::create(&dir.path().join("a.sqlite"), settings()).unwrap();
    let mut slow = mmc_fixtures::create(&dir.path().join("b.sqlite"), settings()).unwrap();
    let map = quick.maps()[0].id;
    for c in strokes() {
        quick.apply(map, c.clone()).unwrap();
        slow.apply(map, c).unwrap();
        slow.wait_for_jobs().unwrap();
    }
    quick.wait_for_jobs().unwrap();
    let expected = all_tiles(&slow, map, SLOPE);
    assert!(all_tiles(&quick, map, SLOPE) == expected);

    quick.rebuild_derived(map).unwrap();
    quick.wait_for_jobs().unwrap();
    assert!(all_tiles(&quick, map, SLOPE) == expected);
    assert!(!quick.poll_jobs().unwrap());
}

#[test]
fn deleting_a_map_stops_its_rebuild_and_undo_rebuilds_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let first = p.maps()[0].id;
    let map = p.add_root_map(settings()).unwrap();
    p.apply(map, stroke(Layer::HEIGHT, 2005.0, 2005.0, 50.0))
        .unwrap();
    p.delete_map(map).unwrap();
    p.wait_for_jobs().unwrap();
    assert!(p.layer_tile(map, SLOPE, 0, 0).is_err());
    // The same edit on a live map gives the expected slope.
    p.apply(first, stroke(Layer::HEIGHT, 2005.0, 2005.0, 50.0))
        .unwrap();
    p.wait_for_jobs().unwrap();
    let flank = slope(&p, first, 2105.0, 2005.0);
    assert!(p.undo_project().unwrap());
    p.wait_for_jobs().unwrap();
    assert_eq!(slope(&p, map, 2105.0, 2005.0), flank);
}
