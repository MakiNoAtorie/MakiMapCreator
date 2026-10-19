use mmc_core::Layer;
use mmc_core::persist::{TileKey, TileRecord, Write};
use mmc_store::{Error, FileReader, Store};

fn key(level: u32) -> TileKey {
    TileKey {
        map_id: 1,
        layer: Layer::Height,
        level,
        tx: 0,
        ty: 0,
    }
}

#[test]
fn reader_sees_committed_tiles_of_the_working_copy_and_the_saved_file() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::create(&tmp.path().join("wc.sqlite")).unwrap();
    store.insert_map(None, "region", 512, 512, 1.0).unwrap();
    let rec = TileRecord {
        data: vec![1, 2, 3],
        edited: false,
    };
    let live = FileReader::open(store.working_copy()).unwrap();
    assert_eq!(live.tile(&key(1)).unwrap(), None);
    store
        .write(&[Write::Tile(key(1), Some(rec.clone()))])
        .unwrap();
    assert_eq!(live.tile(&key(1)).unwrap(), Some(rec.clone()));

    let file = tmp.path().join("p.mmc");
    store.save(&file).unwrap();
    let saved = FileReader::open(&file).unwrap();
    assert_eq!(saved.maps().unwrap(), store.maps().unwrap());
    assert_eq!(saved.tile(&key(1)).unwrap(), Some(rec));
    drop((live, saved));
    store.close().unwrap();
}

#[test]
fn reader_refuses_a_file_that_is_not_a_project() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("x.txt");
    std::fs::write(&file, b"hello").unwrap();
    assert!(matches!(FileReader::open(&file), Err(Error::NotAProject)));
}
