use mmc_core::{MapKind, MapSettings, Project};

#[test]
fn default_region_is_8192_at_10_metres_per_pixel() {
    let s = MapSettings::default_for(MapKind::Region);
    assert_eq!((s.width, s.height, s.metres_per_pixel), (8192, 8192, 10.0));
}

#[test]
fn create_save_reopen_keeps_map_tree_and_settings() {
    let app_data = tempfile::tempdir().unwrap();
    let docs = tempfile::tempdir().unwrap();
    let file = docs.path().join("my world.mmc");
    let settings = MapSettings {
        kind: MapKind::Region,
        width: 2048,
        height: 1024,
        metres_per_pixel: 25.0,
    };

    let project = Project::create(&app_data.path().join("a.sqlite"), settings).unwrap();
    let before = project.maps().to_vec();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].parent, None);
    assert_eq!(before[0].settings, settings);
    project.save(&file).unwrap();
    project.close().unwrap();

    let project = Project::open(&file, &app_data.path().join("b.sqlite")).unwrap();
    assert_eq!(project.maps(), before.as_slice());
    project.close().unwrap();
}
