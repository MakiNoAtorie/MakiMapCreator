//! Live brush (ADR-0025): stamps on a GPU height copy equal the CPU brush within `TOLERANCE`.

use mmc_core::{
    BrushMode, BrushStroke, Command, Detail, Layer, MapId, MapSettings, Project, RootPoint,
    StrokePoint,
};
use mmc_kind_region::{REGION, RegionKind};
use mmc_render::{Live, Renderer, ShaderPass, TARGET_FORMAT, View};

/// Metres. f32 maths and noise on the GPU against f64 on the CPU; measured 2.5e-4 at 800 m.
const TOLERANCE: f32 = 0.001;
const MPP: f64 = 10.0;

fn fixture(dir: &std::path::Path) -> (Project, MapId) {
    let settings = MapSettings {
        kind: REGION,
        width: 600,
        height: 400,
        metres_per_pixel: MPP,
    };
    let mut p = mmc_fixtures::create(&dir.join("wc.sqlite"), settings).unwrap();
    let map = p.maps()[0].id;
    let hill = stroke(&[(3000.0, 2000.0, 1.0)], 1500.0, 800.0);
    p.apply(map, Command::Brush(hill)).unwrap();
    (p, map)
}

fn stroke(points: &[(f64, f64, f32)], radius: f64, strength: f32) -> BrushStroke {
    BrushStroke {
        layer: Layer::HEIGHT,
        points: points
            .iter()
            .map(|&(x, y, pressure)| StrokePoint {
                at: RootPoint { x, y },
                pressure,
                radius,
            })
            .collect(),
        strength,
        falloff: 0.7,
        ..Default::default()
    }
}

fn renderer() -> Renderer {
    let mut r = Renderer::warp().unwrap();
    let pass = ShaderPass::new(r.device(), shader());
    r.register(Layer::HEIGHT, Box::new(pass));
    r
}

fn shader() -> &'static str {
    use mmc_core::MapKindModule;
    RegionKind.render_passes()[0].shader
}

/// 1 map pixel per image pixel over map pixels 100..400 x 50..300: level 0.
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

fn live(r: &Renderer, p: &Project, v: View) -> Live {
    let mut live = Live::new(
        r.device().clone(),
        r.queue().clone(),
        shader(),
        TARGET_FORMAT,
    );
    live.set_base(r.scene(p, v).unwrap().into_base(Layer::HEIGHT).unwrap());
    live
}

fn strokes() -> Vec<BrushStroke> {
    let detail = Detail {
        amount: 0.8,
        scale: 320.0,
    };
    let path: Vec<_> = (0..12)
        .map(|i| {
            (
                1400.0 + i as f64 * 180.0,
                1200.0 + i as f64 * 70.0,
                0.4 + 0.05 * i as f32,
            )
        })
        .collect();
    let raise = BrushStroke {
        detail,
        ..stroke(&path, 400.0, 150.0)
    };
    // Lower to -50 m across the hill; past the view's right edge too, which the copy clips.
    let target = BrushStroke {
        target: Some(-50.0),
        detail,
        ..stroke(
            &[
                (2500.0, 2200.0, 0.9),
                (3600.0, 2400.0, 1.0),
                (4100.0, 2600.0, 1.0),
            ],
            500.0,
            -1.0,
        )
    };
    let roughen = BrushStroke {
        mode: BrushMode::Roughen,
        detail: Detail {
            amount: 1.0,
            ..detail
        },
        ..stroke(&[(2000.0, 1000.0, 1.0)], 600.0, 40.0)
    };
    vec![raise, target, roughen]
}

#[test]
fn gpu_stamps_equal_the_cpu_brush_within_the_tolerance() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = fixture(dir.path());
    let r = renderer();
    let v = view(map);
    let mut live = live(&r, &p, v);
    let before = live.heights().unwrap();
    let seed = p.seed().unwrap();
    for s in strokes() {
        assert!(live.stamp(seed, &s), "{:?} runs on the GPU", s.mode);
        p.apply(map, Command::Brush(s)).unwrap();
    }
    let gpu = live.heights().unwrap();
    let cpu = r.scene(&p, v).unwrap().into_base(Layer::HEIGHT).unwrap();
    assert_eq!(gpu.len(), cpu.values().len());
    let (mut worst, mut changed) = (0.0f32, 0);
    for ((g, c), b) in gpu.iter().zip(cpu.values()).zip(&before) {
        worst = worst.max((g - c).abs());
        changed += (b != c) as usize;
    }
    assert!(changed > 10_000, "the strokes change {changed} pixels");
    assert!(worst <= TOLERANCE, "worst difference {worst} m");
}

#[test]
fn the_copy_shades_like_the_renderer() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = fixture(dir.path());
    let r = renderer();
    let v = view(map);
    let mut live = live(&r, &p, v);
    // Same heights: same bytes.
    let scene = r.scene(&p, v).unwrap();
    assert!(live.render(v).unwrap().rgba == r.render(&scene, u32::MAX).unwrap().rgba);
    let seed = p.seed().unwrap();
    for s in strokes() {
        live.stamp(seed, &s);
        p.apply(map, Command::Brush(s)).unwrap();
    }
    let gpu = live.render(v).unwrap();
    let cpu = r.render(&r.scene(&p, v).unwrap(), u32::MAX).unwrap();
    let worst = gpu
        .rgba
        .iter()
        .zip(&cpu.rgba)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    // No visible jump when the CPU render replaces the copy.
    assert!(worst <= 1, "worst channel difference {worst}");
}

#[test]
fn smooth_and_flatten_stay_on_the_cpu() {
    let dir = tempfile::tempdir().unwrap();
    let (p, map) = fixture(dir.path());
    let r = renderer();
    let mut live = live(&r, &p, view(map));
    let before = live.heights().unwrap();
    for mode in [BrushMode::Smooth, BrushMode::Flatten] {
        let s = BrushStroke {
            mode,
            ..stroke(&[(2000.0, 2000.0, 1.0)], 500.0, 0.5)
        };
        assert!(!live.stamp(p.seed().unwrap(), &s));
    }
    assert!(live.heights().unwrap() == before);
}
