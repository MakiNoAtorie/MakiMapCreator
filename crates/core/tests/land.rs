use mmc_core::{Command, Layer, MapKind, MapSettings, PaintStroke, Project, RootPoint};

fn world() -> MapSettings {
    MapSettings {
        kind: MapKind::World,
        width: 512,
        height: 512,
        metres_per_pixel: 1000.0,
    }
}

fn paint(x: f64, y: f64, value: f32) -> Command {
    Command::Paint(PaintStroke {
        layer: Layer::Land,
        points: vec![RootPoint { x, y }],
        radius: 50_000.0,
        value,
    })
}

#[test]
fn paint_sets_land_and_sea_and_undoes() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Project::create(&dir.path().join("wc.sqlite"), world()).unwrap();
    let map = p.maps()[0].id;
    let land = |p: &Project, x: f64| {
        p.layer_value(map, Layer::Land, RootPoint { x, y: 100_000.0 })
            .unwrap()
            .unwrap()
    };
    assert!(!Layer::Land.is_derived());
    assert_eq!(land(&p, 100_000.0), 0.0);
    p.apply(map, paint(100_000.0, 100_000.0, 1.0)).unwrap();
    p.apply(map, paint(100_000.0, 100_000.0, 1.0)).unwrap();
    assert_eq!(land(&p, 100_000.0), 1.0, "painting twice does not add up");
    assert_eq!(land(&p, 160_000.0), 0.0, "outside the radius stays sea");
    p.apply(map, paint(120_000.0, 100_000.0, 0.0)).unwrap();
    assert_eq!(land(&p, 120_000.0), 0.0);
    assert_eq!(land(&p, 60_000.0), 1.0);
    assert_eq!(p.edited_tiles(map, Layer::Land).unwrap(), [(0, 0)]);
    p.undo(map).unwrap();
    assert_eq!(land(&p, 120_000.0), 1.0);
}
