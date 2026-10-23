use mmc_core::{MapSettings, RootPoint};
use mmc_kind_world::{WORLD, WorldKind, paint};
use mmc_render::{Renderer, View};

#[test]
fn a_world_map_draws_land_and_sea_through_its_kind_passes() {
    let dir = tempfile::tempdir().unwrap();
    let settings = MapSettings {
        kind: WORLD,
        width: 200,
        height: 100,
        metres_per_pixel: 1000.0,
    };
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings).unwrap();
    let map = p.maps()[0].id;
    p.apply(
        map,
        paint(
            vec![RootPoint {
                x: 50_000.0,
                y: 50_000.0,
            }],
            40_000.0,
            true,
        ),
    )
    .unwrap();
    let mut r = Renderer::warp().unwrap();
    r.register_kind(&WorldKind);
    let view = View {
        map,
        min: RootPoint { x: 0.0, y: 0.0 },
        max: RootPoint {
            x: 200_000.0,
            y: 100_000.0,
        },
        width: 200,
        height: 100,
    };
    let img = r.render(&r.scene(&p, view).unwrap(), 64).unwrap();
    let px = |x: u32, y: u32| {
        let i = ((y * img.width + x) * 4) as usize;
        <[u8; 4]>::try_from(&img.rgba[i..i + 4]).unwrap()
    };
    let (land, sea) = (px(50, 50), px(150, 50));
    assert!(land[1] > land[2], "land is green: {land:?}");
    assert!(sea[2] > sea[1], "sea is blue: {sea:?}");
    assert_eq!((land[3], sea[3]), (255, 255));
}
