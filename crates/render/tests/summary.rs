//! Proof step 4 (seam 2): the world map draws each child's summary inside its outline.

use mmc_core::{
    Command, Generation, Kinds, Layer, MapKind, MapKindModule, MapSettings, NewEntity, PaintStroke,
    Project, RootPoint,
};
use mmc_kind_region::RegionKind;
use mmc_kind_world::WorldKind;
use mmc_render::{Renderer, View};

const KM: f64 = 1000.0;

fn at(x: f64, y: f64) -> RootPoint {
    RootPoint {
        x: x * KM,
        y: y * KM,
    }
}

#[test]
fn the_world_render_tints_each_child_outline_with_its_summary() {
    let dir = tempfile::tempdir().unwrap();
    let kinds = Kinds::new(vec![Box::new(WorldKind), Box::new(RegionKind)]);
    let settings = MapSettings {
        kind: MapKind::World,
        width: 64,
        height: 64,
        metres_per_pixel: KM,
    };
    let mut p = Project::create(&dir.path().join("wc.sqlite"), settings).unwrap();
    let w = p.maps()[0].id;
    p.apply(
        w,
        Command::Paint(PaintStroke {
            layer: Layer::Land,
            points: vec![at(20.0, 20.0)],
            radius: 15.0 * KM,
            value: 1.0,
        }),
    )
    .unwrap();
    p.apply(
        w,
        Command::Generate(Generation {
            generator: "test".into(),
            seed: 1,
            entities: vec![NewEntity {
                kind: "region".into(),
                name: "Middle".into(),
                outline: vec![
                    at(10.0, 10.0),
                    at(30.0, 10.0),
                    at(30.0, 30.0),
                    at(10.0, 30.0),
                ],
            }],
            rasters: vec![],
        }),
    )
    .unwrap();
    let e = p.outlines(w).unwrap()[0].0;
    let child = p.create_child_map(&kinds, w, e, Some(500.0)).unwrap();
    let cmd = RegionKind.generate(&p, child, "from-parent", 3).unwrap();
    p.apply(child, cmd).unwrap();
    let tint = p.child_summaries(&kinds, w).unwrap()[0].summary.tint;

    let mut r = Renderer::warp().unwrap();
    r.register_kind(&WorldKind);
    let view = View {
        map: w,
        min: at(0.0, 0.0),
        max: at(64.0, 64.0),
        width: 64,
        height: 64,
    };
    let plain = r.render(&r.scene(&p, view).unwrap(), 64).unwrap();
    let scene = r
        .scene(&p, view)
        .unwrap()
        .with_summaries(&p, &kinds)
        .unwrap();
    let img = r.render(&scene, 64).unwrap();
    let px = |img: &mmc_render::Image, x: u32, y: u32| {
        let i = ((y * img.width + x) * 4) as usize;
        <[u8; 4]>::try_from(&img.rgba[i..i + 4]).unwrap()
    };
    // Inside the outline: the land colour blended with the tint.
    let (base, got) = (px(&plain, 20, 20), px(&img, 20, 20));
    let a = tint[3] as u32;
    for c in 0..3 {
        let want = (base[c] as u32 * (255 - a) + tint[c] as u32 * a + 127) / 255;
        assert_eq!(got[c] as u32, want, "channel {c}");
    }
    // Outside the outline: unchanged.
    assert_eq!(px(&img, 40, 40), px(&plain, 40, 40));
    assert_eq!(px(&img, 5, 20), px(&plain, 5, 20));
    // Tiled equals single.
    assert_eq!(r.render(&scene, 16).unwrap(), img);
}
