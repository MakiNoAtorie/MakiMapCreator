//! Terrain shading: palette in metres, hillshade from root-space slopes, coastline at 0 m.

use mmc_core::{
    BrushStroke, Command, Layer, MapId, MapSettings, PaintStroke, Project, RootPoint, StrokePoint,
};
use mmc_kind_region::{REGION, RegionKind};
use mmc_render::{COAST, Image, PALETTE, Renderer, View};

fn region(dir: &std::path::Path) -> (Project, MapId) {
    let settings = MapSettings {
        kind: REGION,
        width: 600,
        height: 400,
        metres_per_pixel: 10.0,
    };
    let p = mmc_fixtures::create(&dir.join("wc.sqlite"), settings).unwrap();
    let map = p.maps()[0].id;
    (p, map)
}

fn paint(p: &mut Project, map: MapId, x: f64, y: f64, radius: f64, value: f32) {
    let s = PaintStroke {
        layer: Layer::HEIGHT,
        points: vec![RootPoint { x, y }],
        radius,
        value,
    };
    p.apply(map, Command::Paint(s)).unwrap();
}

fn hill(p: &mut Project, map: MapId) {
    let s = BrushStroke {
        layer: Layer::HEIGHT,
        points: vec![StrokePoint {
            at: RootPoint {
                x: 3000.0,
                y: 2000.0,
            },
            pressure: 1.0,
            radius: 1500.0,
        }],
        strength: 1500.0,
        falloff: 1.0,
        target: None,
        ..Default::default()
    };
    p.apply(map, Command::Brush(s)).unwrap();
}

fn view(map: MapId, min: (f64, f64), max: (f64, f64), width: u32, height: u32) -> View {
    View {
        map,
        min: RootPoint { x: min.0, y: min.1 },
        max: RootPoint { x: max.0, y: max.1 },
        width,
        height,
    }
}

fn renderer() -> Renderer {
    let mut r = Renderer::warp().unwrap();
    r.register_kind(&RegionKind);
    r
}

fn pixel(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.width + x) * 4) as usize;
    img.rgba[i..i + 4].try_into().unwrap()
}

/// The palette colour at `h`, as the shader blends it, in bytes.
fn expected(h: f32) -> [u8; 3] {
    let i = PALETTE.iter().position(|s| h <= s.0).unwrap();
    let (a, b) = (PALETTE[i - 1], PALETTE[i]);
    let k = (h - a.0) / (b.0 - a.0);
    std::array::from_fn(|c| ((a.1[c] + (b.1[c] - a.1[c]) * k) * 255.0).round() as u8)
}

fn close(a: [u8; 4], b: [u8; 3], tol: u8) -> bool {
    (0..3).all(|c| a[c].abs_diff(b[c]) <= tol)
}

#[test]
fn flat_ground_takes_the_palette_colour_of_its_height() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = region(dir.path());
    paint(&mut p, map, 1500.0, 2000.0, 1000.0, 1000.0);
    paint(&mut p, map, 4500.0, 2000.0, 1000.0, -3000.0);
    let r = renderer();
    let img = r
        .render(
            &r.scene(&p, view(map, (0.0, 0.0), (6000.0, 4000.0), 600, 400))
                .unwrap(),
            u32::MAX,
        )
        .unwrap();
    for (x, y, h) in [(150, 200, 1000.0), (450, 200, -3000.0), (300, 20, 0.0)] {
        let px = pixel(&img, x, y);
        assert!(close(px, expected(h), 1), "{h} m: {px:?}");
    }
    let (land, sea) = (pixel(&img, 150, 200), pixel(&img, 450, 200));
    assert!(sea[2] > sea[0] && land[0] > land[2], "{land:?} {sea:?}");
}

#[test]
fn the_palette_is_keyed_in_metres_and_sea_is_at_or_below_zero() {
    assert!(PALETTE.windows(2).all(|w| w[0].0 <= w[1].0));
    let zero: Vec<_> = PALETTE.iter().filter(|s| s.0 == 0.0).collect();
    assert_eq!(zero.len(), 2, "one sea stop and one land stop at 0 m");
    let (sea, land) = (zero[0].1, zero[1].1);
    assert!(sea[2] > sea[0] && land[1] > land[2], "{sea:?} {land:?}");
}

#[test]
fn hillshade_does_not_change_with_zoom() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = region(dir.path());
    hill(&mut p, map);
    let r = renderer();
    let whole = r
        .render(
            &r.scene(&p, view(map, (0.0, 0.0), (6000.0, 4000.0), 600, 400))
                .unwrap(),
            u32::MAX,
        )
        .unwrap();
    // 5 image pixels per map pixel; image pixel 2 + 5k has its centre on map pixel 200 + k.
    let zoom = r
        .render(
            &r.scene(&p, view(map, (2000.0, 1500.0), (3200.0, 2300.0), 600, 400))
                .unwrap(),
            u32::MAX,
        )
        .unwrap();
    for (mx, my) in [(220, 170), (260, 210), (300, 180), (240, 225)] {
        let a = pixel(&whole, mx, my);
        let b = pixel(&zoom, 2 + 5 * (mx - 200), 2 + 5 * (my - 150));
        assert!(close(b, [a[0], a[1], a[2]], 1), "({mx}, {my}): {a:?} {b:?}");
    }
    // Light from the north-west.
    let lum = |c: [u8; 4]| c[0] as u32 + c[1] as u32 + c[2] as u32;
    let (nw, se) = (pixel(&whole, 260, 160), pixel(&whole, 340, 240));
    assert!(lum(nw) > lum(se) + 60, "{nw:?} {se:?}");
}

#[test]
fn hillshade_does_not_change_with_pyramid_level() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = region(dir.path());
    hill(&mut p, map);
    let file = dir.path().join("p.mmc");
    p.save(&file);
    p.wait_for_save().unwrap();
    p.close().unwrap();
    let p = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    let r = renderer();
    let v = view(map, (0.0, 0.0), (6000.0, 4000.0), 300, 200);
    r.scene_cached(&p, v).unwrap();
    p.wait_tiles();
    let level1 = r.scene_cached(&p, v).unwrap();
    assert!(level1.is_complete());
    let level1 = r.render(&level1, u32::MAX).unwrap();
    let level0 = r.render(&r.scene(&p, v).unwrap(), u32::MAX).unwrap();
    assert_ne!(level1.rgba, level0.rgba, "drawn from two levels");
    for (x, y) in [(110, 85), (130, 105), (150, 90), (120, 112)] {
        let (a, b) = (pixel(&level0, x, y), pixel(&level1, x, y));
        assert!(close(b, [a[0], a[1], a[2]], 6), "({x}, {y}): {a:?} {b:?}");
    }
}

/// Pixels of row `y` nearer the coast colour than the ground either side of it.
fn coast_pixels(img: &Image, y: u32) -> Vec<u32> {
    let coast = COAST.map(|c| (c * 255.0).round() as i32);
    let dist =
        |c: [u8; 4], d: [i32; 3]| -> i32 { (0..3).map(|i| (c[i] as i32 - d[i]).abs()).sum() };
    let ends = [pixel(img, 0, y), pixel(img, img.width - 1, y)];
    (0..img.width)
        .filter(|&x| {
            let c = pixel(img, x, y);
            ends.iter()
                .all(|e| dist(c, coast) < dist(c, [e[0] as i32, e[1] as i32, e[2] as i32]))
        })
        .collect()
}

#[test]
fn the_coastline_is_about_one_screen_pixel_wide_at_every_zoom() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = region(dir.path());
    paint(&mut p, map, 3000.0, 2000.0, 9000.0, -500.0);
    hill(&mut p, map);
    let r = renderer();
    // Row through the hill top; its lit west coast crosses 0 m near x = 2025 m.
    for (min, max, w) in [
        ((2.0, 1900.0), (3002.0, 2100.0), 300),
        ((1825.0, 1990.0), (2225.0, 2010.0), 400),
        ((1975.0, 1997.5), (2075.0, 2002.5), 800),
    ] {
        let img = r
            .render(&r.scene(&p, view(map, min, max, w, 20)).unwrap(), 64)
            .unwrap();
        let line = coast_pixels(&img, 10);
        assert!(
            (1..=2).contains(&line.len()),
            "{w} px over {} m: {line:?}",
            max.0 - min.0
        );
        // Antialiased: the line blends into the ground either side.
        let x = line[0];
        let (a, b, c) = (
            pixel(&img, x - 2, 10),
            pixel(&img, x - 1, 10),
            pixel(&img, x, 10),
        );
        assert!(a != b || b != c, "edge has no blend: {a:?} {b:?} {c:?}");
    }
}

#[test]
fn flat_sea_at_zero_has_no_coastline() {
    let dir = tempfile::tempdir().unwrap();
    let (p, map) = region(dir.path());
    let r = renderer();
    let img = r
        .render(
            &r.scene(&p, view(map, (0.0, 0.0), (6000.0, 4000.0), 60, 40))
                .unwrap(),
            u32::MAX,
        )
        .unwrap();
    let sea = expected(0.0);
    assert!(
        img.rgba
            .chunks(4)
            .all(|c| close(c.try_into().unwrap(), sea, 1))
    );
}

#[test]
fn sea_floor_lowered_from_zero_has_no_coastline_and_islands_raised_from_it_have_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map) = region(dir.path());
    let stamp = |x: f64, strength: f32| BrushStroke {
        layer: Layer::HEIGHT,
        points: vec![StrokePoint {
            at: RootPoint { x, y: 2000.0 },
            pressure: 1.0,
            radius: 1000.0,
        }],
        strength,
        falloff: 1.0,
        ..Default::default()
    };
    p.apply(map, Command::Brush(stamp(1500.0, -40.0))).unwrap();
    p.apply(map, Command::Brush(stamp(4500.0, 40.0))).unwrap();
    let r = renderer();
    let img = r
        .render(
            &r.scene(&p, view(map, (0.0, 0.0), (6000.0, 4000.0), 600, 400))
                .unwrap(),
            u32::MAX,
        )
        .unwrap();
    let line = coast_pixels(&img, 200);
    assert!(line.iter().all(|&x| x > 300), "line in the sea: {line:?}");
    assert!(line.iter().any(|&x| x > 300), "island has no coast");
}
