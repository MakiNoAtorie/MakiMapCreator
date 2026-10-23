use mmc_core::{BrushStroke, Command, Layer, MapSettings, RootPoint, StrokePoint, TILE_SIZE};
use mmc_kind_region::REGION;

#[test]
fn layer_tile_returns_a_whole_tile_matching_point_reads() {
    let dir = tempfile::tempdir().unwrap();
    let settings = MapSettings {
        kind: REGION,
        width: 512,
        height: 512,
        metres_per_pixel: 1.0,
    };
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), settings).unwrap();
    let map = p.maps()[0].id;
    let at = RootPoint { x: 300.5, y: 40.5 };
    p.apply(
        map,
        Command::Brush(BrushStroke {
            layer: Layer::HEIGHT,
            points: vec![StrokePoint {
                at,
                pressure: 1.0,
                radius: 20.0,
            }],
            strength: 100.0,
            falloff: 1.0,
            target: None,
        }),
    )
    .unwrap();

    let tile = p.layer_tile(map, Layer::HEIGHT, 1, 0).unwrap();
    assert_eq!(tile.len(), (TILE_SIZE * TILE_SIZE) as usize);
    let i = 40 * TILE_SIZE as usize + (300 - TILE_SIZE as usize);
    assert!(tile[i] > 0.0);
    assert_eq!(
        Some(tile[i]),
        p.layer_value(map, Layer::HEIGHT, at).unwrap()
    );
    assert!(
        p.layer_tile(map, Layer::HEIGHT, 0, 1)
            .unwrap()
            .iter()
            .all(|&v| v == 0.0)
    );
}
