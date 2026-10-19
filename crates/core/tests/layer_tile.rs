use mmc_core::{
    BrushStroke, Command, Layer, MapKind, MapSettings, Project, RootPoint, StrokePoint, TILE_SIZE,
};

#[test]
fn layer_tile_returns_a_whole_tile_matching_point_reads() {
    let dir = tempfile::tempdir().unwrap();
    let settings = MapSettings {
        kind: MapKind::Region,
        width: 512,
        height: 512,
        metres_per_pixel: 1.0,
    };
    let mut p = Project::create(&dir.path().join("wc.sqlite"), settings).unwrap();
    let map = p.maps()[0].id;
    let at = RootPoint { x: 300.5, y: 40.5 };
    p.apply(
        map,
        Command::Brush(BrushStroke {
            layer: Layer::Height,
            points: vec![StrokePoint { at, pressure: 1.0 }],
            radius: 20.0,
            strength: 100.0,
        }),
    )
    .unwrap();

    let tile = p.layer_tile(map, Layer::Height, 1, 0).unwrap();
    assert_eq!(tile.len(), (TILE_SIZE * TILE_SIZE) as usize);
    let i = 40 * TILE_SIZE as usize + (300 - TILE_SIZE as usize);
    assert!(tile[i] > 0.0);
    assert_eq!(
        Some(tile[i]),
        p.layer_value(map, Layer::Height, at).unwrap()
    );
    assert!(
        p.layer_tile(map, Layer::Height, 0, 1)
            .unwrap()
            .iter()
            .all(|&v| v == 0.0)
    );
}
