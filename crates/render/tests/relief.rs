//! Erosion relief (ADR-0026): the live copy's GPU relief equals the CPU stage within `TOLERANCE`.

use mmc_core::{
    BrushStroke, Command, Detail, Layer, MapId, MapKindModule, MapSettings, Project, RootPoint,
    StrokePoint, relief_margin,
};
use mmc_kind_region::{REGION, RELIEF, RegionKind};
use mmc_render::{FlattenLevel, Live, Renderer, TARGET_FORMAT, View};

/// Metres. f32 on the GPU against f64 on the CPU on the same f32 heights; measured 1.8e-3 m.
const TOLERANCE: f32 = 0.01;
const MPP: f64 = 10.0;
/// The copy's width: the view's 300 pixels and 2 of margin each side.
const W: usize = 304;

fn stroke(points: &[(f64, f64)], radius: f64, strength: f32) -> BrushStroke {
    BrushStroke {
        layer: Layer::HEIGHT,
        points: points
            .iter()
            .map(|&(x, y)| StrokePoint {
                at: RootPoint { x, y },
                pressure: 1.0,
                radius,
            })
            .collect(),
        strength,
        falloff: 1.0,
        detail: Detail {
            amount: 0.5,
            scale: 320.0,
        },
        ..Default::default()
    }
}

fn fixture(dir: &std::path::Path) -> (Project, MapId) {
    let settings = MapSettings {
        kind: REGION,
        width: 600,
        height: 400,
        metres_per_pixel: MPP,
    };
    let mut p = mmc_fixtures::create(&dir.join("wc.sqlite"), settings).unwrap();
    let map = p.maps()[0].id;
    let hill = stroke(&[(3000.0, 2000.0)], 1800.0, 1400.0);
    p.apply(map, Command::Brush(hill)).unwrap();
    (p, map)
}

/// Map pixels 100..400 x 50..300 at level 0.
fn view(map: MapId) -> View {
    View {
        map,
        min: RootPoint {
            x: 1000.0,
            y: 500.0,
        },
        max: RootPoint {
            x: 4000.0,
            y: 3000.0,
        },
        width: 300,
        height: 250,
    }
}

/// Height plus relief from the CPU: what the shading draws.
fn cpu(r: &Renderer, p: &mut Project, v: View) -> Vec<f32> {
    p.wait_for_jobs().unwrap();
    let h = r.scene(p, v).unwrap().into_base(Layer::HEIGHT).unwrap();
    let d = r.scene(p, v).unwrap().into_base(RELIEF).unwrap();
    h.values()
        .iter()
        .zip(d.values())
        .map(|(a, b)| a + b)
        .collect()
}

/// Largest difference away from the copy's edge, where the GPU clamps and the CPU reads on.
fn worst(a: &[f32], b: &[f32]) -> f32 {
    let (w, h) = (W, a.len() / W);
    let m = relief_margin(MPP) + 1;
    let mut worst = 0.0f32;
    for y in m..h - m {
        for x in m..w - m {
            worst = worst.max((a[y * w + x] - b[y * w + x]).abs());
        }
    }
    worst
}

#[test]
fn the_live_relief_equals_the_cpu_stage_before_and_after_a_stroke() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = fixture(dir.path());
    let mut r = Renderer::warp().unwrap();
    r.register_kind(&RegionKind);
    let v = view(map);
    let seed = p.seed().unwrap();
    let o = RegionKind.relief(&p.kind_options(REGION).unwrap()).unwrap();
    let shader = RegionKind.render_passes()[0].shader;
    let mut live = Live::new(r.device().clone(), r.queue().clone(), shader, TARGET_FORMAT);
    p.wait_for_jobs().unwrap();
    let base = r.scene(&p, v).unwrap().into_base(Layer::HEIGHT).unwrap();
    assert_eq!(base.values().len() % W, 0);
    live.set_base(base.with_relief(seed, o));
    let want = cpu(&r, &mut p, v);
    let got = live.shaded().unwrap();
    let before = worst(&got, &want);

    // A ridge across the hill, shown live while the CPU applies it.
    let ridge = stroke(
        &[(1800.0, 1400.0), (2600.0, 1900.0), (3400.0, 2300.0)],
        500.0,
        300.0,
    );
    assert!(live.stamp(seed, &ridge, FlattenLevel::Kept));
    p.apply(map, Command::Brush(ridge)).unwrap();
    let want = cpu(&r, &mut p, v);
    let got = live.shaded().unwrap();
    let after = worst(&got, &want);
    let heights = live.heights().unwrap();
    let relief = got.iter().zip(&heights).filter(|(a, b)| a != b).count();
    println!("relief: worst {before} m before, {after} m after; {relief} texels with relief");
    assert!(relief > 10_000, "{relief}");
    assert!(before < TOLERANCE && after < TOLERANCE, "{before} {after}");
}

/// The live copy hands over to a canvas render only when its relief has caught up, so the
/// relief never shows the ground before the stroke.
#[test]
fn a_cached_scene_is_complete_only_once_the_relief_has_caught_up() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = fixture(dir.path());
    let mut r = Renderer::warp().unwrap();
    r.register_kind(&RegionKind);
    let v = view(map);
    p.wait_for_jobs().unwrap();
    let ridge = stroke(&[(1800.0, 1400.0), (3400.0, 2300.0)], 500.0, 300.0);
    p.apply(map, Command::Brush(ridge)).unwrap();
    assert!(p.derived_pending(map));
    assert!(!r.scene_cached(&p, v).unwrap().is_complete());
    p.wait_for_jobs().unwrap();
    assert!(!p.derived_pending(map));
    while !r.scene_cached(&p, v).unwrap().is_complete() {
        p.wait_tiles();
    }
}
