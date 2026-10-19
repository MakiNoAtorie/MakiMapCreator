use std::sync::Arc;

use mmc_core::{
    BrushStroke, Command, Layer, MapId, MapKind, MapSettings, Project, RootPoint, StrokePoint,
    TILE_SIZE, TileId,
};

const T: usize = TILE_SIZE as usize;

fn project(dir: &std::path::Path) -> (Project, MapId) {
    let settings = MapSettings {
        kind: MapKind::Region,
        width: 1000,
        height: 1024,
        metres_per_pixel: 1.0,
    };
    let mut p = mmc_fixtures::create(&dir.join("wc.sqlite"), settings).unwrap();
    let map = p.maps()[0].id;
    // Crosses tile and quadrant borders at every level.
    p.apply(
        map,
        Command::Brush(BrushStroke {
            layer: Layer::Height,
            points: vec![StrokePoint {
                at: RootPoint { x: 512.0, y: 250.0 },
                pressure: 1.0,
            }],
            radius: 90.0,
            strength: 50.0,
        }),
    )
    .unwrap();
    (p, map)
}

fn id(map: MapId, level: u32, tx: u32, ty: u32) -> TileId {
    lid(map, Layer::Height, level, tx, ty)
}

fn lid(map: MapId, layer: Layer, level: u32, tx: u32, ty: u32) -> TileId {
    TileId {
        map,
        layer,
        level,
        tx,
        ty,
    }
}

/// The tile at exactly `level` once loads and pyramid updates are done.
fn full(p: &Project, id: TileId) -> Arc<[f32]> {
    p.wait_tiles();
    p.cached_tile(id).unwrap();
    p.wait_tiles();
    let (l, t) = p.cached_tile(id).unwrap().expect("loaded after wait");
    assert_eq!(l, id.level);
    t
}

/// Each pixel of a level is the mean of the 2x2 pixels below it.
fn assert_pyramid_consistent(p: &Project, map: MapId, layer: Layer) {
    let top = p.top_level(map).unwrap();
    for level in 1..=top {
        let n = 1024 / (T << level) as u32;
        for ty in 0..n.max(1) {
            for tx in 0..n.max(1) {
                let parent = full(p, lid(map, layer, level, tx, ty));
                for (qy, qx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                    let child = full(p, lid(map, layer, level - 1, tx * 2 + qx, ty * 2 + qy));
                    for y in 0..T / 2 {
                        for x in 0..T / 2 {
                            let c = |dx, dy| child[(2 * y + dy) * T + 2 * x + dx];
                            let mean = (c(0, 0) + c(1, 0) + c(0, 1) + c(1, 1)) * 0.25;
                            let got =
                                parent[(qy as usize * T / 2 + y) * T + qx as usize * T / 2 + x];
                            assert!((got - mean).abs() < 1e-3, "level {level} tile {tx},{ty}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn edits_undo_and_redo_update_every_level_above() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    assert_eq!(p.top_level(map).unwrap(), 2);
    assert_pyramid_consistent(&p, map, Layer::Height);
    assert!(full(&p, id(map, 2, 0, 0)).iter().any(|&v| v > 0.0));

    p.undo(map).unwrap();
    assert!(full(&p, id(map, 2, 0, 0)).iter().all(|&v| v == 0.0));
    p.redo(map).unwrap();
    assert_pyramid_consistent(&p, map, Layer::Height);
}

#[test]
fn a_coarser_loaded_level_stands_in_until_the_full_tile_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    let file = dir.path().join("p.mmc");
    p.save(&file);
    p.wait_for_save().unwrap();
    p.close().unwrap();
    let p = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    let top = full(&p, id(map, 2, 0, 0));

    let (level, stand_in) = p.cached_tile(id(map, 0, 1, 1)).unwrap().unwrap();
    assert_eq!(level, 2);
    assert_eq!(stand_in, top);
    p.wait_tiles();
    assert_eq!(p.cached_tile(id(map, 0, 1, 1)).unwrap().unwrap().0, 0);
}

#[test]
fn the_tile_cache_stays_within_its_memory_limit() {
    let dir = tempfile::tempdir().unwrap();
    let (p, map) = project(dir.path());
    let tile_bytes = T * T * 4;
    p.set_tile_cache_limit(3 * tile_bytes);
    for t in 0..4 * 4 {
        p.cached_tile(id(map, 0, t % 4, t / 4)).unwrap();
        p.wait_tiles();
    }
    assert!(p.tile_cache_bytes() <= 3 * tile_bytes);
    assert!(p.tile_cache_bytes() > 0);
    // The most recent tile is kept.
    assert_eq!(p.cached_tile(id(map, 0, 3, 3)).unwrap().unwrap().0, 0);
}

#[test]
fn the_saved_file_has_an_overview_per_map_readable_without_opening() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    let file = dir.path().join("p.mmc");
    p.save(&file);
    p.wait_for_save().unwrap();

    let overviews = mmc_fixtures::read_overviews(&file, Layer::Height).unwrap();
    assert_eq!(overviews.len(), 1);
    let o = &overviews[0];
    assert_eq!(o.map, p.maps()[0]);
    assert_eq!(o.level, p.top_level(map).unwrap());
    assert_eq!(*o.values, *full(&p, id(map, o.level, 0, 0)));
    let wc = dir.path().join("wc.sqlite");
    p.close().unwrap();
    assert!(!wc.exists());
}

#[test]
fn derived_layers_get_pyramids_and_rebuilds_replace_cached_tiles() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    p.wait_for_jobs().unwrap();
    assert_pyramid_consistent(&p, map, Layer::Slope);
    let before = full(&p, lid(map, Layer::Slope, 2, 0, 0));
    assert!(before.iter().any(|&v| v > 0.0));

    p.apply(
        map,
        Command::Brush(BrushStroke {
            layer: Layer::Height,
            points: vec![StrokePoint {
                at: RootPoint { x: 800.0, y: 800.0 },
                pressure: 1.0,
            }],
            radius: 60.0,
            strength: 80.0,
        }),
    )
    .unwrap();
    p.wait_for_jobs().unwrap();
    assert_ne!(full(&p, lid(map, Layer::Slope, 2, 0, 0)), before);
    assert_pyramid_consistent(&p, map, Layer::Slope);
}
