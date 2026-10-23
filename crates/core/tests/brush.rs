//! Detail brushes and the smooth, flatten and roughen modes.

use mmc_core::{
    BrushMode, BrushStroke, Command, Detail, Layer, MapId, MapSettings, Project, RootPoint,
    StrokePoint, stamp_weight,
};
use mmc_kind_region::REGION;

const MPP: f64 = 10.0;

fn project(dir: &std::path::Path) -> (Project, MapId) {
    let p = mmc_fixtures::create(
        &dir.join("wc.sqlite"),
        MapSettings {
            kind: REGION,
            width: 768,
            height: 768,
            metres_per_pixel: MPP,
        },
    )
    .unwrap();
    let map = p.maps()[0].id;
    (p, map)
}

fn pt(x: f64, y: f64) -> RootPoint {
    RootPoint { x, y }
}

/// Root-space centre of pixel (`i`, `j`) on a map at the origin.
fn px(i: i64, j: i64) -> RootPoint {
    pt((i as f64 + 0.5) * MPP, (j as f64 + 0.5) * MPP)
}

fn stroke(mode: BrushMode, points: &[RootPoint], radius: f64, strength: f32) -> BrushStroke {
    BrushStroke {
        layer: Layer::HEIGHT,
        points: points
            .iter()
            .map(|&at| StrokePoint {
                at,
                pressure: 1.0,
                radius,
            })
            .collect(),
        strength,
        falloff: 1.0,
        target: None,
        mode,
        detail: Detail::NONE,
    }
}

fn h(p: &Project, map: MapId, at: RootPoint) -> f32 {
    p.layer_value(map, Layer::HEIGHT, at).unwrap().unwrap()
}

/// Heights of the pixels in `i0..i1` x `j0..j1`.
fn grid(p: &Project, map: MapId, i: std::ops::Range<i64>, j: std::ops::Range<i64>) -> Vec<f32> {
    j.flat_map(|j| i.clone().map(move |i| (i, j)))
        .map(|(i, j)| h(p, map, px(i, j)))
        .collect()
}

const DETAIL: Detail = Detail {
    amount: 0.8,
    scale: 400.0,
};

#[test]
fn detail_applies_the_published_stamp_weight() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    let c = pt(2560.0, 2560.0);
    let mut s = stroke(BrushMode::Add, &[c], 800.0, 10.0);
    s.detail = DETAIL;
    p.apply(map, Command::Brush(s)).unwrap();
    let seed = p.seed().unwrap();
    let mut ring = vec![];
    for j in 170..342 {
        for i in 170..342 {
            let at = px(i, j);
            let want = 10.0 * stamp_weight(at, c, 800.0, 1.0, DETAIL, seed, MPP);
            assert_eq!(h(&p, map, at), want, "pixel ({i}, {j})");
            let d = ((at.x - c.x).powi(2) + (at.y - c.y).powi(2)).sqrt();
            if (d - 400.0).abs() < 5.0 {
                ring.push(want);
            }
        }
    }
    // Same distance, different detail: not a smooth bell.
    let (lo, hi) = ring
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    assert!(hi - lo > 1.0, "ring {lo}..{hi}");
}

#[test]
fn detail_zero_is_the_smooth_bell_and_detail_keeps_the_sign() {
    let c = pt(0.0, 0.0);
    let at = pt(300.0, 100.0);
    let d2: f64 = (300.0f64.powi(2) + 100.0f64.powi(2)) / 800.0f64.powi(2);
    let bell = ((1.0 - d2) * (1.0 - d2)) as f32;
    let w = stamp_weight(at, c, 800.0, 1.0, Detail::NONE, 5, MPP);
    assert!((w - bell).abs() < 1e-6, "{w} {bell}");
    assert_eq!(
        stamp_weight(pt(900.0, 0.0), c, 800.0, 1.0, DETAIL, 5, MPP),
        0.0
    );
    let full = Detail {
        amount: 1.0,
        scale: 50.0,
    };
    for i in 0..2000 {
        let at = pt(i as f64 * 0.37 - 370.0, i as f64 * 0.11 - 100.0);
        let w = stamp_weight(at, c, 800.0, 0.5, full, 9, MPP);
        assert!((0.0..=2.0).contains(&w), "{w}");
    }
}

#[test]
fn detail_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    let mut s = stroke(
        BrushMode::Add,
        &[pt(2000.0, 2600.0), pt(2600.0, 2400.0)],
        600.0,
        -15.0,
    );
    s.detail = DETAIL;
    p.apply(map, Command::Brush(s.clone())).unwrap();
    let first = grid(&p, map, 130..330, 170..330);
    assert!(first.iter().all(|&v| v <= 0.0), "lowering never raises");
    assert!(p.undo(map).unwrap());
    for part in s.points.chunks(1) {
        let mut s = s.clone();
        s.points = part.to_vec();
        p.apply_part(map, Command::Brush(s)).unwrap();
    }
    p.end_stroke(map).unwrap();
    assert_eq!(grid(&p, map, 130..330, 170..330), first);
}

/// A 1000 m spike across the tile border at x = 256 px.
fn spike(p: &mut Project, map: MapId) {
    let mut s = stroke(BrushMode::Add, &[px(256, 300)], 60.0, 1000.0);
    s.falloff = 0.0;
    p.apply(map, Command::Brush(s)).unwrap();
}

#[test]
fn smooth_lowers_a_spike_on_both_sides_of_a_tile_border_in_one_undo_step() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    spike(&mut p, map);
    let before = grid(&p, map, 236..276, 280..320);
    for part in [[px(250, 300)], [px(262, 300)]] {
        let s = stroke(BrushMode::Smooth, &part, 400.0, 1.0);
        p.apply_part(map, Command::Brush(s)).unwrap();
    }
    p.end_stroke(map).unwrap();
    assert!(h(&p, map, px(256, 300)) < 900.0);
    // Next to the spike on each side, outside it, ground rises.
    for i in [249, 263] {
        let v = h(&p, map, px(i, 300));
        assert!(v > 50.0 && v < 1000.0, "pixel {i}: {v}");
    }
    let l = h(&p, map, px(252, 300));
    let r = h(&p, map, px(259, 300));
    assert!((l - r).abs() < 0.25 * l.max(r), "left {l} right {r}");
    assert!(p.undo(map).unwrap());
    assert_eq!(grid(&p, map, 236..276, 280..320), before);
}

#[test]
fn flatten_moves_ground_toward_the_height_under_the_pen_at_stroke_start() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    // Terraces rising 10 m per 100 m toward x = 4000 m: 150 m at the start, 200 m at x = 3000 m.
    for i in 0..30 {
        let mut s = stroke(
            BrushMode::Add,
            &[pt(4000.0, 3000.0)],
            3000.0 - i as f64 * 100.0,
            10.0,
        );
        s.falloff = 0.0;
        p.apply(map, Command::Brush(s)).unwrap();
    }
    let start = pt(2455.0, 3005.0);
    let level = h(&p, map, start);
    let (low, high) = (
        h(&p, map, pt(2255.0, 3005.0)),
        h(&p, map, pt(2955.0, 3005.0)),
    );
    assert_eq!((low, level, high), (130.0, 150.0, 200.0));
    let before = grid(&p, map, 180..320, 280..320);
    for part in [vec![start], vec![pt(2705.0, 3005.0), pt(2955.0, 3005.0)]] {
        let s = stroke(BrushMode::Flatten, &part, 600.0, 0.5);
        p.apply_part(map, Command::Brush(s)).unwrap();
    }
    p.end_stroke(map).unwrap();
    // Toward the start level from both sides, never past it.
    let after = h(&p, map, pt(2955.0, 3005.0));
    assert!(after < high - 10.0 && after >= level, "{after}");
    let below = h(&p, map, pt(2255.0, 3005.0));
    assert!(below > low && below <= level, "{below}");
    assert!(p.undo(map).unwrap());
    assert_eq!(grid(&p, map, 180..320, 280..320), before);
}

#[test]
fn roughen_adds_detail_without_a_net_raise() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    let mut s = stroke(BrushMode::Roughen, &[pt(3840.0, 3840.0)], 3800.0, 20.0);
    s.falloff = 0.0;
    s.detail = Detail {
        amount: 1.0,
        scale: 160.0,
    };
    p.apply(map, Command::Brush(s)).unwrap();
    // A hard disc over about 2000 noise cells.
    let every = |n: usize| (120..648).step_by(n);
    let g: Vec<f32> = every(3)
        .flat_map(|j| every(3).map(move |i| px(i, j)))
        .map(|at| h(&p, map, at))
        .collect();
    let mean = g.iter().map(|&v| v as f64).sum::<f64>() / g.len() as f64;
    let size = g.iter().map(|&v| v.abs() as f64).sum::<f64>() / g.len() as f64;
    assert!(size > 3.0, "mean change {size}");
    assert!(mean.abs() < 0.15 * size, "mean {mean} of {size}");
    assert!(p.undo(map).unwrap());
    assert!(grid(&p, map, 380..390, 380..390).iter().all(|&v| v == 0.0));
}

#[test]
fn detail_tables_give_the_bits_of_detail_noise() {
    let (seed, scale) = (0x5EED_u64, 320.0);
    // Negative coordinates, ends not on node multiples.
    let (xs, ys) = ((-37, 90), (1001, 1130));
    let t = mmc_core::detail_tables(seed, scale, MPP, xs, ys);
    for gy in ys.0..=ys.1 {
        for gx in (xs.0..=xs.1).step_by(7) {
            let at = pt((gx as f64 + 0.5) * MPP, (gy as f64 + 0.5) * MPP);
            let want = mmc_core::detail_noise(seed, at, scale, MPP);
            assert_eq!(t.noise_at((gx, gy)).to_bits(), want.to_bits(), "{gx},{gy}");
        }
    }
    // 320 m cells at 10 m: the 20 m octave is under 8 px.
    assert_eq!(t.octaves.len(), 3);
}
