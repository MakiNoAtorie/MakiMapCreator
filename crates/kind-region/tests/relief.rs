//! The derived relief layer (ADR-0026) through the project API.

use mmc_core::{
    BrushStroke, Command, Layer, MapId, MapSettings, Project, RootPoint, StrokePoint, TILE_SIZE,
};
use mmc_kind_region::{REGION, RELIEF, RELIEF_STRENGTH};

const MPP: f64 = 10.0;
const SIZE: u32 = 512;

fn project(dir: &std::path::Path) -> (Project, MapId) {
    let p = mmc_fixtures::create(
        &dir.join("wc.sqlite"),
        MapSettings {
            kind: REGION,
            width: SIZE,
            height: SIZE,
            metres_per_pixel: MPP,
        },
    )
    .unwrap();
    let map = p.maps()[0].id;
    (p, map)
}

/// One smooth stamp of `strength` metres, `radius` metres, at (`x`, `y`) metres.
fn hill(x: f64, y: f64, radius: f64, strength: f32) -> Command {
    Command::Brush(BrushStroke {
        layer: Layer::HEIGHT,
        points: vec![StrokePoint {
            at: RootPoint { x, y },
            pressure: 1.0,
            radius,
        }],
        strength,
        falloff: 1.0,
        ..Default::default()
    })
}

/// The whole map's `layer`, row by row.
fn raster(p: &Project, map: MapId, layer: Layer) -> Vec<f32> {
    let n = SIZE / TILE_SIZE;
    let mut out = vec![0.0; (SIZE * SIZE) as usize];
    for ty in 0..n {
        for tx in 0..n {
            let t = p.layer_tile(map, layer, tx, ty).unwrap();
            for y in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    let i = ((ty * TILE_SIZE + y) * SIZE + tx * TILE_SIZE + x) as usize;
                    out[i] = t[(y * TILE_SIZE + x) as usize];
                }
            }
        }
    }
    out
}

/// A tall hill in the middle and a low one in a corner.
fn hills(p: &mut Project, map: MapId, order: &[usize]) {
    let cmds = [
        hill(2560.0, 2560.0, 1800.0, 1500.0),
        hill(2900.0, 2200.0, 900.0, 400.0),
        hill(800.0, 800.0, 600.0, 15.0),
    ];
    for &i in order {
        p.apply(map, cmds[i].clone()).unwrap();
    }
    p.wait_for_jobs().unwrap();
}

#[test]
fn a_mountain_gets_relief_and_low_ground_none() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    hills(&mut p, map, &[0, 1, 2]);
    let r = raster(&p, map, RELIEF);
    let h = raster(&p, map, Layer::HEIGHT);
    let flank: Vec<f32> = (0..r.len())
        .filter(|&i| h[i] > 600.0 && h[i] < 1400.0)
        .map(|i| r[i])
        .collect();
    let m = flank.iter().map(|x| x.abs()).sum::<f32>() / flank.len() as f32;
    assert!(flank.len() > 1000 && m > 3.0, "{} {m}", flank.len());
    assert!((0..r.len()).filter(|&i| h[i] < 20.0).all(|i| r[i] == 0.0));
}

#[test]
fn relief_depends_on_the_height_only_not_on_stroke_order_or_reruns() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, ma) = project(dir.path());
    hills(&mut a, ma, &[0, 1, 2]);
    let ra = raster(&a, ma, RELIEF);
    for _ in 0..3 {
        a.undo(ma).unwrap();
    }
    hills(&mut a, ma, &[2, 1, 0]);
    let rb = raster(&a, ma, RELIEF);
    let worst = ra
        .iter()
        .zip(&rb)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max);
    assert!(worst < 1e-2, "{worst}");
    a.rebuild_derived(ma).unwrap();
    a.wait_for_jobs().unwrap();
    assert_eq!(raster(&a, ma, RELIEF), ra);
}

#[test]
fn the_strength_option_scales_the_relief() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    hills(&mut p, map, &[0]);
    let before = raster(&p, map, RELIEF);
    p.set_kind_option(REGION, RELIEF_STRENGTH.id, RELIEF_STRENGTH.default * 2.0)
        .unwrap();
    p.wait_for_jobs().unwrap();
    let after = raster(&p, map, RELIEF);
    let sum = |v: &[f32]| v.iter().map(|x| x.abs() as f64).sum::<f64>();
    let k = sum(&after) / sum(&before);
    assert!((k - 2.0).abs() < 1e-3, "{k}");
    assert_eq!(
        p.kind_options(REGION).unwrap().get(&RELIEF_STRENGTH),
        RELIEF_STRENGTH.default * 2.0
    );
}
