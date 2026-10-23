//! Live brush (ADR-0025) on WARP: a part's stamps on the GPU copy to a drawn 1920x1080 frame.

use mmc_core::{Command, Layer, MapId, MapKindModule, Project, RootPoint};
use mmc_fixtures::{LargeMap, Rng};
use mmc_render::{Live, Renderer, ShaderPass, TARGET_FORMAT, View};

use crate::{RUNS, detailed, median, time_ms};

/// (stamp to visible, stroke start to visible): the median ms of one frame's part of 4 stamps,
/// and of the same with the copy's upload at pen down.
pub fn measure(project: &Project, map: MapId, fixture: &LargeMap, rng: &mut Rng) -> (f64, f64) {
    let shader = mmc_kind_region::RegionKind.render_passes()[0].shader;
    let mut r = Renderer::warp().unwrap();
    r.register(Layer::HEIGHT, Box::new(ShaderPass::new(r.device(), shader)));
    let m = fixture.metres_per_pixel;
    let view = View {
        map,
        min: RootPoint {
            x: 20000.0,
            y: 20000.0,
        },
        max: RootPoint {
            x: 20000.0 + 1920.0 * m,
            y: 20000.0 + 1080.0 * m,
        },
        width: 1920,
        height: 1080,
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
                x: view.min.x + 900.0 * m + i as f64 * 10.0 * m,
                y: view.min.y + 500.0 * m,
            };
        }
        b
    };
    // Shaders compile on first use.
    live.set_base(base());
    live.stamp(seed, &part(rng));
    live.draw(view);
    live.wait().unwrap();
    // The stamps run on the GPU, then the frame is submitted. Its draw is any frame's draw:
    // about 20 ms on WARP, under 1 ms on a GPU.
    let stamp = (0..RUNS)
        .map(|_| {
            let b = part(rng);
            let ms = time_ms(|| {
                live.stamp(seed, &b);
                live.wait().unwrap();
                live.draw(view);
            })
            .0;
            live.wait().unwrap();
            ms
        })
        .collect();
    // Up to the frame's submission: WARP runs the upload in about 175 ms, a GPU in about 1.5 ms.
    let start = (0..RUNS)
        .map(|_| {
            let (b, heights) = (part(rng), base());
            let ms = time_ms(|| {
                live.set_base(heights);
                live.stamp(seed, &b);
                live.draw(view);
            })
            .0;
            live.wait().unwrap();
            ms
        })
        .collect();
    (median(stamp), median(start))
}
