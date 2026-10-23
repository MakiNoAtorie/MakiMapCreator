//! Live brush (ADR-0025): a part's stamps on the GPU copy until its frame is drawn.

use std::collections::BTreeMap;

use mmc_core::{Command, Layer, MapId, MapKindModule, Project, RootPoint};
use mmc_fixtures::{LargeMap, Rng};
use mmc_render::{FlattenLevel, Live, Renderer, ShaderPass, TARGET_FORMAT, View};

use crate::{RUNS, detailed, median, time_ms};

/// On WARP at 384x216 (its upload costs 85 ns a texel); `*_gpu` on a hardware adapter at full HD.
pub fn measure(
    project: &Project,
    map: MapId,
    fixture: &LargeMap,
    rng: &mut Rng,
    results: &mut BTreeMap<String, f64>,
) {
    let mut runs = vec![("", Renderer::warp().unwrap(), (384, 216))];
    runs.extend(Renderer::hardware().map(|r| ("_gpu", r, (1920, 1080))));
    for (suffix, r, size) in runs {
        let (stamp, start) = on(r, size, project, map, fixture, rng);
        results.insert(format!("stamp_to_visible{suffix}"), stamp);
        results.insert(format!("stroke_start_to_visible{suffix}"), start);
    }
}

/// Median ms from a 4-stamp part to its drawn frame: (alone, with the upload at pen down).
fn on(
    mut r: Renderer,
    (width, height): (u32, u32),
    project: &Project,
    map: MapId,
    fixture: &LargeMap,
    rng: &mut Rng,
) -> (f64, f64) {
    let shader = mmc_kind_region::RegionKind.render_passes()[0].shader;
    r.register(Layer::HEIGHT, Box::new(ShaderPass::new(r.device(), shader)));
    let m = fixture.metres_per_pixel;
    let view = View {
        map,
        min: RootPoint {
            x: 20000.0,
            y: 20000.0,
        },
        max: RootPoint {
            x: 20000.0 + width as f64 * m,
            y: 20000.0 + height as f64 * m,
        },
        width,
        height,
    };
    let seed = project.seed().unwrap();
    let mut live = Live::new(r.device().clone(), r.queue().clone(), shader, TARGET_FORMAT);
    let base = || {
        r.scene(project, view)
            .unwrap()
            .into_base(Layer::HEIGHT)
            .unwrap()
    };
    let part = |rng: &mut Rng| {
        let Command::Brush(mut b) = detailed(fixture.stroke(rng), m) else {
            unreachable!()
        };
        b.points.truncate(4);
        for (i, p) in b.points.iter_mut().enumerate() {
            p.at = RootPoint {
                x: view.min.x + (width / 2) as f64 * m + i as f64 * 10.0 * m,
                y: view.min.y + (height / 2) as f64 * m,
            };
        }
        b
    };
    // Shaders compile on first use.
    live.set_base(base());
    live.stamp(seed, &part(rng), FlattenLevel::Sample);
    live.draw(view);
    live.wait().unwrap();
    let stamp = (0..RUNS)
        .map(|_| {
            let b = part(rng);
            time_ms(|| {
                live.stamp(seed, &b, FlattenLevel::Sample);
                live.draw(view);
                live.wait().unwrap();
            })
            .0
        })
        .collect();
    let start = (0..RUNS)
        .map(|_| {
            let (b, heights) = (part(rng), base());
            time_ms(|| {
                live.set_base(heights);
                live.stamp(seed, &b, FlattenLevel::Sample);
                live.draw(view);
                live.wait().unwrap();
            })
            .0
        })
        .collect();
    (median(stamp), median(start))
}
