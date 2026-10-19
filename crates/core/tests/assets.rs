use mmc_core::{MapKind, MapSettings, Project};

fn new_project(dir: &std::path::Path) -> Project {
    mmc_fixtures::create(
        &dir.join("wc.sqlite"),
        MapSettings::default_for(MapKind::World),
    )
    .unwrap()
}

#[test]
fn asset_reads_back_by_pack_and_asset_id() {
    let tmp = tempfile::tempdir().unwrap();
    let p = new_project(tmp.path());
    p.add_asset("pack", "tree", b"abc").unwrap();
    assert_eq!(
        p.asset("pack", "tree").unwrap().as_deref(),
        Some(&b"abc"[..])
    );
    assert_eq!(p.asset("pack", "rock").unwrap(), None);
    p.close().unwrap();
}

#[test]
fn same_content_is_stored_once() {
    let tmp = tempfile::tempdir().unwrap();
    let p = new_project(tmp.path());
    p.add_asset("pack", "a", b"same").unwrap();
    p.add_asset("pack", "b", b"same").unwrap();
    p.add_asset("pack", "a", b"same").unwrap();
    assert_eq!(p.asset_blob_count().unwrap(), 1);
    assert_eq!(p.asset("pack", "b").unwrap().as_deref(), Some(&b"same"[..]));
    p.close().unwrap();
}

#[test]
fn newer_content_replaces_the_asset_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let p = new_project(tmp.path());
    p.add_asset("pack", "a", b"v1").unwrap();
    p.add_asset("pack", "a", b"v2").unwrap();
    assert_eq!(p.asset("pack", "a").unwrap().as_deref(), Some(&b"v2"[..]));
    p.close().unwrap();
}

#[test]
fn assets_survive_save_and_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("p.mmc");
    let mut p = new_project(tmp.path());
    p.add_asset("pack", "tree", b"abc").unwrap();
    p.save(&file);
    p.close().unwrap();
    let p = mmc_fixtures::open(&file, &tmp.path().join("wc2.sqlite")).unwrap();
    assert_eq!(
        p.asset("pack", "tree").unwrap().as_deref(),
        Some(&b"abc"[..])
    );
    p.close().unwrap();
}
