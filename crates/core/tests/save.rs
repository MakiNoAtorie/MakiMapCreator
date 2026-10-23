use std::path::Path;

use mmc_core::{
    BrushStroke, Command, Layer, MapId, MapSettings, Project, RootPoint, StrokePoint, TILE_SIZE,
};
use mmc_kind_region::REGION;

const MPP: f64 = 10.0;

fn settings() -> MapSettings {
    MapSettings {
        kind: REGION,
        width: 8192,
        height: 8192,
        metres_per_pixel: MPP,
    }
}

/// The `i`th inner tile corner; a stroke there writes four tiles in one edit.
fn corner(i: usize) -> (f64, f64) {
    let tile = TILE_SIZE as f64 * MPP;
    ((1 + i % 31) as f64 * tile, (1 + i / 31) as f64 * tile)
}

fn stroke_at(i: usize) -> Command {
    let (x, y) = corner(i);
    Command::Brush(BrushStroke {
        layer: Layer::HEIGHT,
        points: vec![StrokePoint {
            at: RootPoint { x, y },
            pressure: 1.0,
            radius: 50.0,
        }],
        strength: 10.0,
        falloff: 1.0,
        target: None,
        ..Default::default()
    })
}

/// Height just inside each of the four tiles that meet at corner `i`.
fn probes(p: &Project, map: MapId, i: usize) -> Vec<f32> {
    let (x, y) = corner(i);
    [(-15.0, -15.0), (15.0, -15.0), (-15.0, 15.0), (15.0, 15.0)]
        .iter()
        .map(|(dx, dy)| {
            p.layer_value(
                map,
                Layer::HEIGHT,
                RootPoint {
                    x: x + dx,
                    y: y + dy,
                },
            )
            .unwrap()
            .unwrap()
        })
        .collect()
}

/// Incompressible filler so that a save takes long enough to overlap edits.
fn bulk(p: &Project) {
    let mut s = 1u64;
    let data: Vec<u8> = (0..48 << 20)
        .map(|_| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
            (s >> 56) as u8
        })
        .collect();
    p.add_asset("bulk", "filler", &data).unwrap();
}

#[test]
fn save_during_edits_writes_a_consistent_snapshot_and_keeps_later_edits() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("world.mmc");
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    bulk(&p);
    let before = 5;
    for i in 0..before {
        p.apply(map, stroke_at(i)).unwrap();
    }

    p.save(&file);
    let mut n = before;
    let mut live = vec![];
    while n < 900 {
        p.apply(map, stroke_at(n)).unwrap();
        live.push(probes(&p, map, n));
        n += 1;
        if let Some(res) = p.poll_save() {
            res.unwrap();
            break;
        }
    }
    p.wait_for_save().unwrap();
    assert!(n > before, "no edit ran during the save");

    // Live project keeps every edit.
    for i in 0..n {
        assert!(
            probes(&p, map, i).iter().all(|h| *h > 0.0),
            "stroke {i} lost"
        );
    }
    p.close().unwrap();

    // Saved file holds whole edits in order: a prefix 0..k, nothing after, nothing partial.
    let q = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    let saved: Vec<Vec<f32>> = (0..n).map(|i| probes(&q, map, i)).collect();
    let k = saved
        .iter()
        .take_while(|v| v.iter().all(|h| *h > 0.0))
        .count();
    assert!(k >= before, "edits before the save are missing");
    for (i, v) in saved.iter().enumerate().skip(k) {
        assert_eq!(
            v, &[0.0; 4],
            "stroke {i} partly or out of order in the snapshot"
        );
    }
    for i in before..k {
        assert_eq!(
            saved[i],
            live[i - before],
            "stroke {i} differs from the snapshot"
        );
    }
    q.close().unwrap();
}

#[test]
fn a_second_save_overwrites_the_first_with_newer_edits() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("world.mmc");
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings()).unwrap();
    let map = p.maps()[0].id;
    p.apply(map, stroke_at(0)).unwrap();
    p.save(&file);
    p.apply(map, stroke_at(1)).unwrap();
    p.save(&file);
    p.close().unwrap();

    let q = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert!(probes(&q, map, 1).iter().all(|h| *h > 0.0));
    q.close().unwrap();
}

fn copy_db(from: &Path, to: &Path) {
    for suffix in ["", "-wal"] {
        let f = format!("{}{suffix}", from.display());
        if Path::new(&f).exists() {
            std::fs::copy(&f, format!("{}{suffix}", to.display())).unwrap();
        }
    }
}

#[test]
fn a_leftover_working_copy_restores_unsaved_edits() {
    let dir = tempfile::tempdir().unwrap();
    let wc = dir.path().join("wc.sqlite");
    let crashed = dir.path().join("crashed.sqlite");
    let mut p = mmc_fixtures::create(&wc, settings()).unwrap();
    let map = p.maps()[0].id;
    p.apply(map, stroke_at(3)).unwrap();
    p.wait_for_jobs().unwrap();
    // Files as a crash leaves them: the database and its WAL, never closed.
    copy_db(&wc, &crashed);
    p.close().unwrap();

    let mut r = mmc_fixtures::restore(&crashed).unwrap();
    assert_eq!(r.maps().len(), 1);
    assert!(probes(&r, map, 3).iter().all(|h| *h > 0.0));
    r.apply(map, stroke_at(4)).unwrap();
    let file = dir.path().join("world.mmc");
    r.save(&file);
    r.close().unwrap();
    assert!(!crashed.exists());

    let q = mmc_fixtures::open(&file, &wc).unwrap();
    assert!(probes(&q, map, 4).iter().all(|h| *h > 0.0));
    q.close().unwrap();
}

#[test]
fn a_discarded_working_copy_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let wc = dir.path().join("wc.sqlite");
    let crashed = dir.path().join("crashed.sqlite");
    let mut p = mmc_fixtures::create(&wc, settings()).unwrap();
    let map = p.maps()[0].id;
    p.apply(map, stroke_at(0)).unwrap();
    p.wait_for_jobs().unwrap();
    copy_db(&wc, &crashed);
    p.close().unwrap();

    mmc_fixtures::discard_working_copy(&crashed).unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
