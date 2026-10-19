use mmc_core::Layer;
use mmc_core::persist::{FieldDefRecord, GenerationRecord, TileKey, TileRecord, Write};
use mmc_store::Store;

#[test]
fn a_write_batch_is_one_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(&dir.path().join("wc.sqlite")).unwrap();
    let map = store.insert_map(None, "region", 256, 256, 10.0).unwrap();
    let key = TileKey {
        map_id: map,
        layer: Layer::HEIGHT,
        level: 0,
        tx: 0,
        ty: 0,
    };
    let tile = Write::Tile(
        key.clone(),
        Some(TileRecord {
            data: vec![1],
            edited: false,
        }),
    );
    let generation = Write::Generation(
        GenerationRecord {
            id: 1,
            map_id: map,
            generator: "g".into(),
            seed: 1,
            params: "{}".into(),
            app_version: "0".into(),
        },
        true,
    );
    // The field definition names no entity kind, so the batch fails at its last row.
    let bad = Write::FieldDef(
        FieldDefRecord {
            kind: "no-such-kind".into(),
            key: "k".into(),
            label: "K".into(),
            field_type: "text".into(),
            choices: None,
            builtin: false,
        },
        true,
    );
    assert!(
        store
            .write(&[tile.clone(), generation.clone(), bad])
            .is_err()
    );
    assert_eq!(store.tile(&key).unwrap(), None);
    assert!(store.generations(map).unwrap().is_empty());
    store.write(&[tile, generation]).unwrap();
    assert!(store.tile(&key).unwrap().is_some());
    assert_eq!(store.generations(map).unwrap().len(), 1);
}
