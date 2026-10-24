//! Detail brushes and the smooth, flatten and roughen modes.

use mmc_core::{
    BrushMode, BrushStroke, COAST_BAND, Command, Detail, Layer, MapId, MapSettings, PaintStroke,
    Project, RootPoint, StrokePoint, WATER_LEVEL, brush_weight, stamp_weight,
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

/// The detail noise a raising add uses at `at` on ground at `height`.
fn noise(seed: u64, at: RootPoint, d: Detail, height: f32) -> f64 {
    mmc_core::detail_noise(seed, at, d.scale, MPP).at(height, true)
}

#[test]
fn raising_high_ground_takes_ridged_detail() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    let c = pt(2560.0, 2560.0);
    let plateau = PaintStroke {
        layer: Layer::HEIGHT,
        points: vec![c],
        radius: 2000.0,
        value: 2000.0,
    };
    p.apply(map, Command::Paint(plateau)).unwrap();
    let mut s = stroke(BrushMode::Add, &[c], 800.0, 10.0);
    s.detail = DETAIL;
    p.apply(map, Command::Brush(s)).unwrap();
    let seed = p.seed().unwrap();
    let mut ridged = 0;
    for j in (170..342).step_by(3) {
        for i in (170..342).step_by(3) {
            let at = px(i, j);
            let n = mmc_core::detail_noise(seed, at, DETAIL.scale, MPP);
            assert_eq!(n.at(2000.0, true), mmc_core::RIDGE_GAIN * n.ridged);
            assert_eq!(n.at(100.0, true), n.value);
            assert_eq!(n.at(2000.0, false), n.value);
            let want = 2000.0 + 10.0 * stamp_weight(at, c, 800.0, 1.0, DETAIL, n.at(2000.0, true));
            assert_eq!(h(&p, map, at), want, "pixel ({i}, {j})");
            ridged += (n.ridged != n.value) as usize;
        }
    }
    assert!(ridged > 1000, "{ridged}");
}

#[test]
fn detail_applies_the_published_stamp_weight() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    let c = pt(2560.0, 2560.0);
    // Sea floor, away from the water level, where detail fades.
    let floor = PaintStroke {
        layer: Layer::HEIGHT,
        points: vec![c],
        radius: 2000.0,
        value: -500.0,
    };
    p.apply(map, Command::Paint(floor)).unwrap();
    let mut s = stroke(BrushMode::Add, &[c], 800.0, 10.0);
    s.detail = DETAIL;
    p.apply(map, Command::Brush(s)).unwrap();
    let seed = p.seed().unwrap();
    let mut ring = vec![];
    for j in 170..342 {
        for i in 170..342 {
            let at = px(i, j);
            let n = noise(seed, at, DETAIL, -500.0);
            let want = -500.0 + 10.0 * stamp_weight(at, c, 800.0, 1.0, DETAIL, n);
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
    let t = 1.0 - d2;
    let bell = (t * t * (3.0 - 2.0 * t)) as f32;
    let w = stamp_weight(at, c, 800.0, 1.0, Detail::NONE, 0.0);
    assert!((w - bell).abs() < 1e-6, "{w} {bell}");
    assert_eq!(
        stamp_weight(pt(900.0, 0.0), c, 800.0, 1.0, DETAIL, 1.0),
        0.0
    );
    let full = Detail {
        amount: 1.0,
        scale: 50.0,
    };
    for i in 0..2000 {
        let at = pt(i as f64 * 0.37 - 370.0, i as f64 * 0.11 - 100.0);
        let w = stamp_weight(at, c, 800.0, 0.5, full, noise(9, at, full, 5000.0));
        assert!(w > 0.0 && w < 2.0, "{w}");
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
        assert_eq!(p.stroke_level(map), Some(level));
    }
    p.end_stroke(map).unwrap();
    assert_eq!(p.stroke_level(map), None);
    // Toward the start level from both sides, never past it.
    let after = h(&p, map, pt(2955.0, 3005.0));
    assert!(after < high - 10.0 && after >= level, "{after}");
    let below = h(&p, map, pt(2255.0, 3005.0));
    assert!(below > low && below <= level, "{below}");
    assert!(p.undo(map).unwrap());
    assert_eq!(grid(&p, map, 180..320, 280..320), before);
}

#[test]
fn roughen_adds_detail_without_a_net_raise_in_one_undo_step() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    let roughen = |at| BrushStroke {
        falloff: 0.0,
        detail: Detail {
            amount: 1.0,
            scale: 160.0,
        },
        ..stroke(BrushMode::Roughen, &[at], 3800.0, 10.0)
    };
    let plateau = PaintStroke {
        layer: Layer::HEIGHT,
        points: vec![pt(3840.0, 3840.0)],
        radius: 6000.0,
        value: 500.0,
    };
    p.apply(map, Command::Paint(plateau)).unwrap();
    // Two parts of one stroke at the same place: 20 m of noise.
    for _ in 0..2 {
        p.apply_part(map, Command::Brush(roughen(pt(3840.0, 3840.0))))
            .unwrap();
    }
    p.end_stroke(map).unwrap();
    // A hard disc over about 2000 noise cells.
    let every = |n: usize| (120..648).step_by(n);
    let g: Vec<f32> = every(3)
        .flat_map(|j| every(3).map(move |i| px(i, j)))
        .map(|at| h(&p, map, at))
        .collect();
    let mean = g.iter().map(|&v| v as f64 - 500.0).sum::<f64>() / g.len() as f64;
    let size = g.iter().map(|&v| (v - 500.0).abs() as f64).sum::<f64>() / g.len() as f64;
    assert!(size > 3.0, "mean change {size}");
    assert!(mean.abs() < 0.15 * size, "mean {mean} of {size}");
    assert!(p.undo(map).unwrap());
    assert!(
        grid(&p, map, 380..390, 380..390)
            .iter()
            .all(|&v| v == 500.0)
    );
}

#[test]
fn detail_tables_give_detail_noise() {
    let (seed, scale) = (0x5EED_u64, 320.0);
    // Negative coordinates, ends not on node multiples.
    let (xs, ys) = ((-37, 90), (1001, 1130));
    let t = mmc_core::detail_tables(seed, scale, MPP, xs, ys);
    for gy in ys.0..=ys.1 {
        for gx in (xs.0..=xs.1).step_by(7) {
            let at = pt((gx as f64 + 0.5) * MPP, (gy as f64 + 0.5) * MPP);
            let want = mmc_core::detail_noise(seed, at, scale, MPP);
            let got = t.noise_at((gx, gy));
            let err = (got.value - want.value)
                .abs()
                .max((got.ridged - want.ridged).abs());
            assert!(err < 1e-8, "{gx},{gy}: {got:?} {want:?}");
        }
    }
    // 320 m cells at 10 m: from the 39.4 m octave on, cells are under 7 px.
    assert_eq!(t.octaves.len(), 3);
}

/// Value and ridged detail noise at `n` pixels from (`x`, `y`) in steps of (`dx`, `dy`) px.
fn line(seed: u64, (x, y): (f64, f64), (dx, dy): (f64, f64), n: usize) -> Vec<(f64, f64)> {
    line_at(seed, 320.0, (x, y), (dx, dy), n)
}

fn line_at(
    seed: u64,
    scale: f64,
    (x, y): (f64, f64),
    (dx, dy): (f64, f64),
    n: usize,
) -> Vec<(f64, f64)> {
    (0..n)
        .map(|i| {
            let at = pt(
                (x + dx * i as f64 + 0.5) * MPP,
                (y + dy * i as f64 + 0.5) * MPP,
            );
            let d = mmc_core::detail_noise(seed, at, scale, MPP);
            (d.value, d.ridged)
        })
        .collect()
}

#[test]
fn detail_noise_has_no_flat_runs_and_no_clamp() {
    let v = line(11, (-5000.0, 300.0), (1.0, 0.0), 20_000);
    for w in v.windows(2) {
        assert!(w[0].0 != w[1].0 && w[0].1 != w[1].1, "{w:?}");
    }
    let spread = |f: fn(&(f64, f64)) -> f64| {
        let m = v.iter().map(f).sum::<f64>() / v.len() as f64;
        let sd = (v.iter().map(|x| (f(x) - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        (m, sd)
    };
    for (name, (mean, sd)) in [("value", spread(|x| x.0)), ("ridged", spread(|x| x.1))] {
        assert!(
            mean.abs() < 0.15 && (0.3..0.6).contains(&sd),
            "{name}: {mean} {sd}"
        );
    }
    // A stronger noise always weighs more: no clamp makes a flat top.
    let full = Detail {
        amount: 1.0,
        scale: 320.0,
    };
    let w: Vec<f32> = (-40..=40)
        .map(|i| {
            stamp_weight(
                pt(100.0, 0.0),
                pt(0.0, 0.0),
                800.0,
                1.0,
                full,
                i as f64 * 0.1,
            )
        })
        .collect();
    assert!(w.windows(2).all(|p| p[0] < p[1]), "{w:?}");
}

#[test]
fn detail_noise_has_no_axis_aligned_grain() {
    // Mean squared step of the value noise along 0, 90, 45 and 135 degrees, per pixel of distance.
    let step = |d: (f64, f64)| {
        let v = line(5, (-3000.0, -2000.0), d, 40_000);
        let len2 = d.0 * d.0 + d.1 * d.1;
        v.windows(2).map(|w| (w[1].0 - w[0].0).powi(2)).sum::<f64>() / v.len() as f64 / len2
    };
    let s = [
        step((1.0, 0.0)),
        step((0.0, 1.0)),
        step((1.0, 1.0)),
        step((1.0, -1.0)),
    ];
    let (lo, hi) = s
        .iter()
        .fold((f64::MAX, 0.0f64), |(a, b), &x| (a.min(x), b.max(x)));
    assert!(hi < 1.15 * lo, "{s:?}");
}

#[test]
fn ridged_detail_is_smooth_in_its_valleys_and_rough_on_its_crests() {
    // Roughness: the ridged noise's difference from the mean of its neighbours 8 px away, with
    // all six octaves.
    let mut pairs = vec![];
    for j in 0..60 {
        let v = line_at(3, 2000.0, (0.0, j as f64 * 37.0), (1.0, 0.0), 2000);
        for i in (8..v.len() - 8).step_by(5) {
            let rough = (v[i].1 - 0.5 * (v[i - 8].1 + v[i + 8].1)).abs();
            pairs.push((v[i].1, rough));
        }
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let q = pairs.len() / 4;
    let mean = |s: &[(f64, f64)]| s.iter().map(|p| p.1).sum::<f64>() / s.len() as f64;
    let (low, high) = (mean(&pairs[..q]), mean(&pairs[pairs.len() - q..]));
    assert!(low < 0.6 * high, "valleys {low} crests {high}");
}

#[test]
fn the_brush_falloff_has_no_crease_at_its_core() {
    for falloff in [0.3f32, 0.7, 1.0] {
        let core = mmc_core::brush_core(falloff);
        let e = 1e-3;
        let w = brush_weight(core + e, falloff);
        assert!(1.0 - w < 1e-4, "falloff {falloff}: {w}");
        assert!(brush_weight(1.0 - e, falloff) < 1e-4);
    }
}

#[test]
fn detail_fades_out_at_the_water_level() {
    let n = mmc_core::detail_noise(4, pt(1234.0, 567.0), 320.0, MPP);
    assert!(n.value.abs() > 1e-3);
    assert_eq!(n.at(WATER_LEVEL, false), 0.0);
    assert_eq!(n.at(WATER_LEVEL, true), 0.0);
    assert_eq!(n.at(WATER_LEVEL - COAST_BAND, false), n.value);
    assert_eq!(n.at(WATER_LEVEL + COAST_BAND, true), n.value);
    let half = n.at(WATER_LEVEL + 0.5 * COAST_BAND, false);
    assert!(half.abs() < n.value.abs() && half * n.value > 0.0);
}

#[test]
fn roughen_and_detail_never_make_or_drown_land() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = project(dir.path());
    // Shore: sea at -30 m left of x = 3840 m, land at 30 m right of it, smoothed to a ramp.
    for (x, value) in [(840.0, -30.0), (6840.0, 30.0)] {
        let half = PaintStroke {
            layer: Layer::HEIGHT,
            points: vec![pt(x, 3840.0)],
            radius: 3000.0,
            value,
        };
        p.apply(map, Command::Paint(half)).unwrap();
    }
    let line: Vec<_> = (0..12)
        .map(|k| pt(3840.0, 2800.0 + k as f64 * 200.0))
        .collect();
    for _ in 0..3 {
        p.apply(
            map,
            Command::Brush(stroke(BrushMode::Smooth, &line, 900.0, 1.0)),
        )
        .unwrap();
    }
    let (xs, ys) = (200..568, 320..440);
    let detail = Detail {
        amount: 1.0,
        scale: 200.0,
    };
    // Lowering a little with full detail: the coast moves, but leaves no speckle of islands.
    let mut lower = stroke(BrushMode::Add, &[pt(3840.0, 3800.0)], 1600.0, -3.0);
    lower.detail = detail;
    p.apply(map, Command::Brush(lower)).unwrap();
    let before = grid(&p, map, xs.clone(), ys.clone());
    for row in before.chunks(368) {
        let crossings = row
            .windows(2)
            .filter(|w| (w[0] > WATER_LEVEL) != (w[1] > WATER_LEVEL))
            .count();
        assert!(crossings <= 1, "{crossings} coast crossings in a row");
    }
    let land = |g: &[f32]| g.iter().map(|&v| v > WATER_LEVEL).collect::<Vec<_>>();
    let rough = BrushStroke {
        falloff: 0.5,
        detail,
        ..stroke(BrushMode::Roughen, &[pt(3840.0, 3800.0)], 1600.0, 300.0)
    };
    p.apply(map, Command::Brush(rough)).unwrap();
    let after = grid(&p, map, xs, ys);
    assert_eq!(land(&after), land(&before), "roughen moved the coast");
    let changed = after.iter().zip(&before).filter(|(a, b)| a != b).count();
    assert!(changed > after.len() / 2, "roughen changed {changed}");
}
