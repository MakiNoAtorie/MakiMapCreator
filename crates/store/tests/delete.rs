use mmc_core::Layer;
use mmc_core::persist::{EntityRecord, PageRecord, TileKey, TileRecord, Write};
use mmc_store::Store;
use rusqlite::Connection;

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn open_purges_deleted_maps_and_rows_of_deleted_entities() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(&dir.path().join("wc.sqlite")).unwrap();
    let kept = store.insert_map(None, "region", 256, 256, 10.0).unwrap();
    let gone = store.insert_map(None, "region", 256, 256, 10.0).unwrap();
    let tile = |map_id| TileKey {
        map_id,
        layer: Layer::Height,
        level: 0,
        tx: 0,
        ty: 0,
    };
    let rec = TileRecord {
        data: vec![1],
        edited: true,
    };
    store
        .write(&[
            Write::Tile(tile(kept), Some(rec.clone())),
            Write::Tile(tile(gone), Some(rec)),
        ])
        .unwrap();
    let entity = |id| EntityRecord {
        id,
        kind: "region".into(),
        name: "A".into(),
        fields: "{}".into(),
    };
    store
        .write(&[
            Write::Entity(1, Some(entity(1))),
            Write::Entity(2, Some(entity(2))),
        ])
        .unwrap();
    for id in [1, 2] {
        store
            .write(&[Write::Page(PageRecord {
                entity_id: id,
                body: Some("text".into()),
                links: vec![],
            })])
            .unwrap();
    }
    store
        .write(&[Write::MapDeleted(gone, true), Write::Entity(2, None)])
        .unwrap();
    let file = dir.path().join("p.mmc");
    store.save(&file).unwrap();
    store.close().unwrap();

    let wc = dir.path().join("wc2.sqlite");
    let _store = Store::open(&file, &wc).unwrap();
    let conn = Connection::open(&wc).unwrap();
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM maps"), 1);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM tiles"), 1);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM pages"), 1);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM fts_pages"), 1);
}
