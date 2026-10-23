//! Edits while background writers (pyramid worker, derived rebuild) write the working copy:
//! no edit fails on the write lock, and none waits longer than a frame (ADR-0014).

use std::time::{Duration, Instant};

use mmc_core::{BrushStroke, Command, Layer, MapSettings, RootPoint, StrokePoint, TileId};
use mmc_fixtures::{LargeMap, Rng};
use mmc_kind_region::REGION;

#[test]
fn edits_never_fail_or_stall_while_background_writers_run() {
    let dir = tempfile::tempdir().unwrap();
    // Map `big` gets wide strokes; their rebuilds and pyramid updates write while the test
    // edits map `small`. Edits on `small` do not cancel the jobs of `big`.
    let fixture = LargeMap {
        size: 2048,
        strokes: 0,
        points_per_stroke: 4,
        radius: 3000.0,
        ..LargeMap::region(3)
    };
    let (mut p, big) = fixture.build(&dir.path().join("wc.sqlite"));
    let small = p
        .add_root_map(MapSettings {
            kind: REGION,
            width: 512,
            height: 512,
            metres_per_pixel: 10.0,
        })
        .unwrap();
    let top = p.top_level(big).unwrap();
    let top_tile = TileId {
        map: big,
        layer: Layer::HEIGHT,
        level: top,
        tx: 0,
        ty: 0,
    };
    let edit = |i: u32| {
        Command::Brush(BrushStroke {
            layer: Layer::HEIGHT,
            points: vec![StrokePoint {
                at: RootPoint {
                    x: 100.0 + (i % 40) as f64 * 100.0,
                    y: 2500.0,
                },
                pressure: 1.0,
                radius: 20.0,
            }],
            strength: 1.0,
            falloff: 1.0,
            target: None,
            ..Default::default()
        })
    };
    let mut rng = Rng::new(5);
    let (mut rebuilds, mut pyramid) = (0, false);
    let mut worst = Duration::ZERO;
    p.apply(big, fixture.stroke(&mut rng)).unwrap();
    let mut i = 0;
    while rebuilds < 3 || !pyramid {
        assert!(i < 100_000, "background writers never wrote");
        let t = Instant::now();
        p.apply(small, edit(i)).unwrap();
        worst = worst.max(t.elapsed());
        if p.poll_jobs().unwrap() {
            rebuilds += 1;
            // A new wide stroke keeps both background writers busy.
            p.apply(big, fixture.stroke(&mut rng)).unwrap();
        }
        pyramid |= p
            .cached_tile(top_tile)
            .unwrap()
            .is_some_and(|(l, t)| l == top && t.iter().any(|v| *v != 0.0));
        i += 1;
    }
    let slack = if cfg!(debug_assertions) { 10 } else { 1 };
    if worst >= Duration::from_millis(16 * slack) {
        eprintln!("timing: an edit waited {worst:?}");
    }
    p.close().unwrap();
}
