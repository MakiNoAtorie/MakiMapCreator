use std::sync::Arc;

use mmc_core::MapKindModule;
use mmc_core::{
    BrushStroke, Command, Layer, MapId, MapSettings, Project, RootPoint, StrokePoint, TileId,
};
use mmc_kind_region::{REGION, RegionKind};
use mmc_render::{Image, Renderer, ShaderPass, View, export_png};

/// BLAKE3 of the RGBA bytes of `fixture_view()` under WARP. Hashes are per backend.
const WARP_HASH: &str = "7da03512c4be8aaea6d3814623f714fcf857b012f01a5059f98d444c9cd96288";

fn fixture(dir: &std::path::Path) -> (Project, MapId) {
    let settings = MapSettings {
        kind: REGION,
        width: 600,
        height: 400,
        metres_per_pixel: 10.0,
    };
    let mut p = mmc_fixtures::create(&dir.join("wc.sqlite"), settings).unwrap();
    let map = p.maps()[0].id;
    for (x, y, r, s) in [
        (2000.0, 1500.0, 1200.0, 1500.0),
        (4000.0, 2500.0, 900.0, 2500.0),
        (3000.0, 2000.0, 400.0, -800.0),
    ] {
        p.apply(
            map,
            Command::Brush(BrushStroke {
                layer: Layer::HEIGHT,
                points: vec![StrokePoint {
                    at: RootPoint { x, y },
                    pressure: 1.0,
                    radius: r,
                }],
                strength: s,
                falloff: 1.0,
            }),
        )
        .unwrap();
    }
    (p, map)
}

fn view(map: MapId, width: u32, height: u32) -> View {
    View {
        map,
        min: RootPoint { x: 0.0, y: 0.0 },
        max: RootPoint {
            x: 6000.0,
            y: 4000.0,
        },
        width,
        height,
    }
}

fn renderer() -> Renderer {
    let mut r = Renderer::warp().unwrap();
    let pass = ShaderPass::new(r.device(), RegionKind.render_passes()[0].shader);
    r.register(Layer::HEIGHT, Box::new(pass));
    r
}

fn pixel(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.width + x) * 4) as usize;
    img.rgba[i..i + 4].try_into().unwrap()
}

#[test]
fn renders_a_map_area_at_a_chosen_resolution_with_no_window() {
    let dir = tempfile::tempdir().unwrap();
    let (p, map) = fixture(dir.path());
    let r = renderer();
    let img = r
        .render(&r.scene(&p, view(map, 300, 200)).unwrap(), u32::MAX)
        .unwrap();
    assert_eq!((img.width, img.height), (300, 200));
    assert_eq!(img.rgba.len(), 300 * 200 * 4);
    let sea = pixel(&img, 290, 10);
    assert!(sea[2] > sea[1] && sea[2] > sea[0], "sea is blue: {sea:?}");
    assert_eq!(sea[3], 255);
    let hill = pixel(&img, 100, 75);
    assert!(hill[2] < hill[1], "land is not blue: {hill:?}");
    // Light from the north-west: the north-west slope is brighter than the south-east one.
    let (nw, se) = (pixel(&img, 80, 55), pixel(&img, 120, 95));
    let lum = |c: [u8; 4]| c[0] as u32 + c[1] as u32 + c[2] as u32;
    assert!(lum(nw) > lum(se), "hillshade: {nw:?} vs {se:?}");
}

#[test]
fn a_renderer_without_draw_passes_draws_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (p, map) = fixture(dir.path());
    let r = Renderer::warp().unwrap();
    let img = r
        .render(&r.scene(&p, view(map, 64, 32)).unwrap(), u32::MAX)
        .unwrap();
    assert!(img.rgba.iter().all(|&b| b == 0));
}

#[test]
fn a_tiled_render_equals_a_single_render() {
    let dir = tempfile::tempdir().unwrap();
    let (p, map) = fixture(dir.path());
    let r = renderer();
    // Odd sizes and a non-integer scale so tile edges fall mid-pixel in map space.
    for v in [view(map, 451, 307), view(map, 1333, 901)] {
        let scene = r.scene(&p, v).unwrap();
        let single = r.render(&scene, u32::MAX).unwrap();
        let tiled = r.render(&scene, 97).unwrap();
        assert!(
            single.rgba == tiled.rgba,
            "tiled differs at {}x{}",
            v.width,
            v.height
        );
    }
}

#[test]
fn a_fixed_input_gives_a_stable_image_hash() {
    let dir = tempfile::tempdir().unwrap();
    let (p, map) = fixture(dir.path());
    let r = renderer();
    let scene = r.scene(&p, view(map, 320, 240)).unwrap();
    let a = blake3::hash(&r.render(&scene, u32::MAX).unwrap().rgba);
    let b = blake3::hash(&r.render(&scene, 64).unwrap().rgba);
    assert_eq!(a, b);
    assert_eq!(a.to_hex().as_str(), WARP_HASH);
}

#[test]
fn exports_a_png_larger_than_the_screen_on_a_worker() {
    let dir = tempfile::tempdir().unwrap();
    let (p, map) = fixture(dir.path());
    let r = Arc::new(renderer());
    let scene = r.scene(&p, view(map, 4096, 2731)).unwrap();
    let path = dir.path().join("export.png");
    let job = export_png(r.clone(), scene, 1024, path.clone());
    job.join().unwrap().unwrap();

    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
    let info = decoder.read_info().unwrap();
    let info = info.info();
    assert_eq!((info.width, info.height), (4096, 2731));
    assert_eq!(info.color_type, png::ColorType::Rgba);
}

fn reopen(dir: &std::path::Path) -> (Project, MapId) {
    let (mut p, map) = fixture(dir);
    let file = dir.join("p.mmc");
    p.save(&file);
    p.wait_for_save().unwrap();
    p.close().unwrap();
    (
        mmc_fixtures::open(&file, &dir.join("wc2.sqlite")).unwrap(),
        map,
    )
}

#[test]
fn a_cached_scene_draws_a_stand_in_then_the_full_tiles() {
    let dir = tempfile::tempdir().unwrap();
    let (p, map) = reopen(dir.path());
    let r = renderer();
    let top = p.top_level(map).unwrap();
    p.cached_tile(TileId {
        map,
        layer: Layer::HEIGHT,
        level: top,
        tx: 0,
        ty: 0,
    })
    .unwrap();
    p.wait_tiles();

    let v = view(map, 300, 200);
    let stand_in = r.scene_cached(&p, v).unwrap();
    assert!(!stand_in.is_complete());
    let img = r.render(&stand_in, u32::MAX).unwrap();
    assert_eq!(pixel(&img, 290, 10)[3], 255, "the stand-in is drawn");

    p.wait_tiles();
    let full = r.scene_cached(&p, v).unwrap();
    assert!(full.is_complete());
    // 2 map pixels per image pixel: drawn from level 1, not level 0.
    let level0 = r.render(&r.scene(&p, v).unwrap(), u32::MAX).unwrap();
    assert_ne!(r.render(&full, u32::MAX).unwrap().rgba, level0.rgba);

    let v = view(map, 600, 400);
    r.scene_cached(&p, v).unwrap();
    p.wait_tiles();
    let full = r.scene_cached(&p, v).unwrap();
    assert!(full.is_complete());
    let single = r.render(&r.scene(&p, v).unwrap(), u32::MAX).unwrap();
    assert!(r.render(&full, 64).unwrap().rgba == single.rgba);
}

#[test]
fn the_overview_draws_a_first_frame_before_the_project_opens() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = fixture(dir.path());
    let file = dir.path().join("p.mmc");
    p.save(&file);
    p.wait_for_save().unwrap();
    p.close().unwrap();

    let overviews = mmc_fixtures::read_overviews(&file, Layer::HEIGHT).unwrap();
    let r = renderer();
    let scene = r
        .overview_scene(&overviews[0], view(map, 300, 200))
        .unwrap();
    assert!(!scene.is_complete());
    let img = r.render(&scene, u32::MAX).unwrap();
    let sea = pixel(&img, 290, 10);
    assert!(sea[2] > sea[1] && sea[3] == 255, "sea is blue: {sea:?}");
}

#[test]
fn warp_renderers_made_and_used_on_parallel_threads_do_not_crash() {
    // With one WARP device per renderer, this crashed d3d10warp.dll in most runs.
    let jobs: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                let dir = tempfile::tempdir().unwrap();
                let (p, map) = fixture(dir.path());
                for _ in 0..10 {
                    let r = renderer();
                    r.render(&r.scene(&p, view(map, 64, 64)).unwrap(), u32::MAX)
                        .unwrap();
                }
            })
        })
        .collect();
    for j in jobs {
        j.join().unwrap();
    }
}
