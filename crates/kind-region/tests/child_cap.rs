//! Children of crude world regions stay within `MAX_CHILD_SIZE` (8192 px per side).

use std::time::Instant;

use mmc_core::{Command, Error, GenRun, MAX_CHILD_SIZE, MapKindModule, PaintStroke, RootPoint};
use mmc_kind_region::RegionKind;
use mmc_kind_world::{LAND, WorldKind};

#[test]
fn a_default_scale_child_of_a_world_region_fits_the_cap_and_generates() {
    let dir = tempfile::tempdir().unwrap();
    let mut p =
        mmc_fixtures::create(&dir.path().join("wc.sqlite"), WorldKind.default_settings()).unwrap();
    let w = p.maps()[0].id;
    let paint = PaintStroke {
        layer: LAND,
        points: vec![RootPoint {
            x: 2_048_000.0,
            y: 2_048_000.0,
        }],
        radius: 1_500_000.0,
        value: 1.0,
    };
    p.apply(w, Command::Paint(paint)).unwrap();
    let cmd = WorldKind
        .generate(&p, w, &GenRun::new("regions", 7))
        .unwrap();
    p.apply(w, cmd).unwrap();
    let region = p.outlines(w).unwrap()[0].0;

    // At the declared 10 m/px the region would be far wider than the cap.
    let err = p.create_child_map(w, region, Some(10.0)).unwrap_err();
    assert!(matches!(err, Error::ChildTooLarge), "{err}");
    let child = p.create_child_map(w, region, None).unwrap();
    let s = p.maps().iter().find(|m| m.id == child).unwrap().settings;
    assert!(s.width as f64 <= MAX_CHILD_SIZE && s.height as f64 <= MAX_CHILD_SIZE);
    let steps = (s.metres_per_pixel / 10.0).log2();
    assert_eq!(steps, steps.round(), "{}", s.metres_per_pixel);

    let t = Instant::now();
    let cmd = RegionKind
        .generate(&p, child, &GenRun::new("from-parent", 1))
        .unwrap();
    p.apply(child, cmd).unwrap();
    eprintln!("generated {}x{} in {:?}", s.width, s.height, t.elapsed());
    p.close().unwrap();
}
