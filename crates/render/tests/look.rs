//! The owner's painting tests (#28, #29) as a fixture: sea strokes on 0 m ground, a tall ridge,
//! roughen on lowland and across the shore, soft strokes just above 0 m and lowland of many soft
//! strokes. `MMC_LOOK_DIR=<dir> cargo test -p mmc-render
//! --test look -- --ignored` writes `<MMC_LOOK_PREFIX>-*.png` there, and `*-flat.png` without
//! the erosion relief (ADR-0026).

use mmc_core::MapKindModule;
use mmc_core::{
    BrushMode, BrushStroke, Command, Detail, Layer, MapId, MapSettings, Project, RootPoint,
    StrokePoint,
};
use mmc_fixtures::Rng;
use mmc_kind_region::{REGION, RegionKind};
use mmc_render::{Renderer, ShaderPass, View};

const MPP: f64 = 10.0;
const SIZE: u32 = 1000;
/// The app's defaults: stamps every quarter radius, 320 m detail cells at 0.5.
const SPACING: f64 = 0.25;
const DETAIL: Detail = Detail {
    amount: 0.5,
    scale: 320.0,
};

/// One pass along `path` as the app plans it: `strength` metres at the centre per pass.
fn pass(path: &[(f64, f64)], radius: f64, strength: f32, mode: BrushMode) -> Command {
    let mut points = vec![];
    for w in path.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let n = (len / (SPACING * radius)).ceil().max(1.0) as usize;
        for i in 0..n {
            let t = i as f64 / n as f64;
            points.push(StrokePoint {
                at: RootPoint {
                    x: a.0 + (b.0 - a.0) * t,
                    y: a.1 + (b.1 - a.1) * t,
                },
                pressure: 1.0,
                radius,
            });
        }
    }
    Command::Brush(BrushStroke {
        layer: Layer::HEIGHT,
        points,
        strength: strength * SPACING as f32,
        falloff: 1.0,
        target: None,
        mode,
        detail: Detail {
            scale: if mode == BrushMode::Roughen {
                roughen_scale(radius)
            } else {
                DETAIL.scale
            },
            ..DETAIL
        },
    })
}

/// The app's roughen detail scale for a brush of `radius`.
fn roughen_scale(radius: f64) -> f64 {
    radius
}

pub fn build(dir: &std::path::Path) -> (Project, MapId) {
    let settings = MapSettings {
        kind: REGION,
        width: SIZE,
        height: SIZE,
        metres_per_pixel: MPP,
    };
    let mut p = mmc_fixtures::create(&dir.join("wc.sqlite"), settings).unwrap();
    let map = p.maps()[0].id;
    let mut rng = Rng::new(28);
    let mut at = |lo: f64, hi: f64| lo + (hi - lo) * rng.unit();
    let mut cmds = vec![];
    // Lowland: many soft overlapping raise strokes, a few lower ones.
    for i in 0..70 {
        let start = (at(2200.0, 7800.0), at(2200.0, 7800.0));
        let path: Vec<_> = (0..4)
            .scan(start, |p, _| {
                *p = (p.0 + at(-900.0, 900.0), p.1 + at(-900.0, 900.0));
                Some(*p)
            })
            .collect();
        let (r, s) = (at(400.0, 2500.0), at(20.0, 60.0) as f32);
        let s = if i % 7 == 6 { -s } else { s };
        cmds.push(pass(&path, r, s, BrushMode::Add));
    }
    // Sea strokes on the 0 m floor around the island.
    for &(x, y) in &[
        (1000.0, 8800.0),
        (8900.0, 8600.0),
        (9000.0, 1500.0),
        (600.0, 5000.0),
    ] {
        let path = [(x, y), (x + at(-800.0, 800.0), y + at(-800.0, 800.0))];
        cmds.push(pass(
            &path,
            at(500.0, 900.0),
            -at(20.0, 60.0) as f32,
            BrushMode::Add,
        ));
    }
    // A tall ridge: a Y of many strong passes.
    for _ in 0..12 {
        for path in [
            [(1800.0, 4200.0), (3000.0, 3000.0)],
            [(3000.0, 3000.0), (4200.0, 1700.0)],
            [(3000.0, 3000.0), (2700.0, 1600.0)],
        ] {
            cmds.push(pass(&path, 450.0, 200.0, BrushMode::Add));
        }
    }
    // Roughen scribbled over lowland.
    let zig: Vec<_> = (0..8)
        .map(|i| {
            (
                5800.0 + 250.0 * i as f64,
                if i % 2 == 0 { 3000.0 } else { 4400.0 },
            )
        })
        .collect();
    for _ in 0..3 {
        cmds.push(pass(&zig, 900.0, 60.0, BrushMode::Roughen));
    }
    // Roughen scribbled across the east shore.
    let shore: Vec<_> = (0..8)
        .map(|i| {
            (
                7600.0 + 250.0 * i as f64,
                if i % 2 == 0 { 4400.0 } else { 5600.0 },
            )
        })
        .collect();
    for _ in 0..3 {
        cmds.push(pass(&shore, 600.0, 60.0, BrushMode::Roughen));
    }
    // Soft strokes on the 0 m sea floor that end just above it: 1.5 m and 3 m.
    for (path, s) in [
        ([(400.0, 700.0), (1500.0, 1300.0)], 1.5),
        ([(500.0, 2000.0), (1600.0, 2500.0)], 3.0),
    ] {
        cmds.push(pass(&path, 500.0, s, BrushMode::Add));
    }
    for c in cmds {
        p.apply(map, c).unwrap();
    }
    p.wait_for_jobs().unwrap();
    (p, map)
}

fn view(map: MapId, min: (f64, f64), max: (f64, f64), w: u32) -> View {
    View {
        map,
        min: RootPoint { x: min.0, y: min.1 },
        max: RootPoint { x: max.0, y: max.1 },
        width: w,
        height: w,
    }
}

#[test]
#[ignore]
fn renders_the_owner_fixture() {
    let out = std::path::PathBuf::from(std::env::var("MMC_LOOK_DIR").expect("MMC_LOOK_DIR"));
    let prefix = std::env::var("MMC_LOOK_PREFIX").unwrap_or("look".into());
    let dir = tempfile::tempdir().unwrap();
    let (p, map) = build(dir.path());
    let mut r = Renderer::warp().unwrap();
    r.register_kind(&RegionKind);
    let mut flat = Renderer::warp().unwrap();
    let pass = ShaderPass::new(flat.device(), RegionKind.render_passes()[0].shader);
    flat.register(Layer::HEIGHT, Box::new(pass));
    let full = SIZE as f64 * MPP;
    for (name, min, max, w) in [
        ("overview", (0.0, 0.0), (full, full), 800),
        ("ridge", (1200.0, 1000.0), (4800.0, 4600.0), 900),
        ("roughen", (5000.0, 2200.0), (8200.0, 5400.0), 800),
        ("lowland", (3500.0, 4500.0), (7500.0, 8500.0), 800),
        // Close up, 8 image pixels per map pixel: the shapes of the detail itself.
        ("closeup", (2900.0, 2300.0), (3900.0, 3300.0), 800),
        ("shore", (7000.0, 3500.0), (10000.0, 6500.0), 800),
        ("film", (0.0, 0.0), (3000.0, 3000.0), 800),
    ] {
        for (r, suffix) in [(&r, ""), (&flat, "-flat")] {
            let img = r
                .render(&r.scene(&p, view(map, min, max, w)).unwrap(), 256)
                .unwrap();
            img.write_png(&out.join(format!("{prefix}-{name}{suffix}.png")))
                .unwrap();
        }
    }
    // Zoomed out as on the owner's screen: drawn from pyramid level 2.
    let v = view(map, (0.0, 0.0), (full, full), 250);
    for (r, suffix) in [(&r, ""), (&flat, "-flat")] {
        let scene = loop {
            let s = r.scene_cached(&p, v).unwrap();
            if s.is_complete() {
                break s;
            }
            p.wait_tiles();
        };
        let img = r.render(&scene, 256).unwrap();
        img.write_png(&out.join(format!("{prefix}-far{suffix}.png")))
            .unwrap();
    }
}
