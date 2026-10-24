//! Erosion relief (ADR-0026): the reference filter on plain rasters.

use mmc_core::{ReliefOptions, relief, relief_margin};

const MPP: f64 = 10.0;
const N: usize = 160;
const OPTS: ReliefOptions = ReliefOptions {
    strength: 60.0,
    slope: 0.4,
    height: 400.0,
};

/// Heights `f(x, y)` in metres at global pixel centres, x and y in metres.
fn raster(g0: (i64, i64), w: usize, h: usize, f: impl Fn(f64, f64) -> f64) -> Vec<f32> {
    let mut v = Vec::with_capacity(w * h);
    for j in 0..h {
        for i in 0..w {
            let (x, y) = (
                (g0.0 + i as i64) as f64 * MPP + 5.0,
                (g0.1 + j as i64) as f64 * MPP + 5.0,
            );
            v.push(f(x, y) as f32);
        }
    }
    v
}

fn run(g0: (i64, i64), w: usize, h: usize, f: impl Fn(f64, f64) -> f64) -> Vec<f32> {
    relief(7, &OPTS, MPP, g0, w, h, &raster(g0, w, h, f))
}

fn mean_abs(v: &[f32]) -> f64 {
    v.iter().map(|x| x.abs() as f64).sum::<f64>() / v.len() as f64
}

#[test]
fn flat_ground_sea_and_the_coast_get_no_relief() {
    for f in [
        &(|_: f64, _: f64| 1500.0) as &dyn Fn(f64, f64) -> f64,
        &|x, _| -2000.0 + 0.8 * x,
    ] {
        assert!(run((0, 0), N, N, f).iter().all(|&d| d == 0.0));
    }
    // Ground below COAST_BAND never changes, so the coast stays where height puts it.
    let r = run((0, 0), N, N, |x, _| 0.0125 * x);
    assert!(r.iter().all(|&d| d == 0.0), "{}", mean_abs(&r));
}

#[test]
fn steep_high_slopes_get_gullies_down_the_slope() {
    let r = run((0, 0), N, N, |x, _| 1000.0 + 0.6 * x);
    let m = mean_abs(&r);
    assert!(m > 5.0, "mean {m}");
    assert!(r.iter().all(|d| (d.abs() as f64) <= OPTS.strength));
    // Gullies run along the slope (x): the relief changes less along x than across it.
    let (mut along, mut across) = (0.0, 0.0);
    for j in 8..N - 8 {
        for i in 8..N - 8 {
            along += (r[j * N + i + 1] - r[j * N + i]).abs() as f64;
            across += (r[(j + 1) * N + i] - r[j * N + i]).abs() as f64;
        }
    }
    assert!(across > 1.3 * along, "along {along} across {across}");
}

#[test]
fn gentle_and_low_slopes_get_less_relief() {
    let steep = mean_abs(&run((0, 0), N, N, |x, _| 1000.0 + 0.6 * x));
    let gentle = mean_abs(&run((0, 0), N, N, |x, _| 1000.0 + 0.08 * x));
    let low = mean_abs(&run((0, 0), N, N, |x, _| 60.0 + 0.6 * (x % 80.0)));
    assert!(gentle < 0.5 * steep, "gentle {gentle} steep {steep}");
    assert!(low < 0.5 * steep, "low {low} steep {steep}");
}

#[test]
fn a_window_gets_the_relief_of_the_whole_away_from_its_edge() {
    let f = |x: f64, y: f64| 900.0 + 0.5 * x + 300.0 * (y / 400.0).sin();
    let n = 224;
    let whole = run((-40, 13), n, n, f);
    let g0 = (-40 + 37, 13 + 51);
    let (w, h) = (130, 120);
    let part = run(g0, w, h, f);
    let m = relief_margin(MPP);
    for j in m..h - m {
        for i in m..w - m {
            assert_eq!(part[j * w + i], whole[(j + 51) * n + i + 37], "({i}, {j})");
        }
    }
}

#[test]
fn the_seed_changes_the_gullies_only() {
    let v = raster((0, 0), N, N, |x, _| 1000.0 + 0.6 * x);
    let a = relief(1, &OPTS, MPP, (0, 0), N, N, &v);
    let b = relief(2, &OPTS, MPP, (0, 0), N, N, &v);
    assert_eq!(a, relief(1, &OPTS, MPP, (0, 0), N, N, &v));
    assert_ne!(a, b);
    let (ma, mb) = (mean_abs(&a), mean_abs(&b));
    assert!((ma - mb).abs() < 0.3 * ma, "{ma} {mb}");
}
