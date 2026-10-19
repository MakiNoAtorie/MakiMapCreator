use std::path::Path;

use mmc_store::{APPLICATION_ID, Error, MIGRATIONS, SCHEMA_VERSION, Store};
use rusqlite::Connection;

fn dir_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn save_leaves_only_the_project_file_and_sets_header() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("world.mmc");
    let store = Store::create(&tmp.path().join("wc.sqlite")).unwrap();
    store.insert_map(None, "region", 8192, 8192, 10.0).unwrap();

    store.save(&file).unwrap();
    store.save(&file).unwrap(); // overwrite an existing file
    assert_eq!(dir_names(out.path()), ["world.mmc"]);

    let conn = Connection::open(&file).unwrap();
    let app_id: i32 = conn
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .unwrap();
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(app_id, APPLICATION_ID);
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn open_leaves_no_files_next_to_the_project() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("world.mmc");
    let store = Store::create(&tmp.path().join("a.sqlite")).unwrap();
    let id = store.insert_map(None, "region", 100, 50, 2.5).unwrap();
    store.save(&file).unwrap();
    store.close().unwrap();

    let store = Store::open(&file, &tmp.path().join("b.sqlite")).unwrap();
    store
        .insert_map(Some(id), "settlement", 10, 10, 0.5)
        .unwrap();
    assert_eq!(dir_names(out.path()), ["world.mmc"]);
    let maps = store.maps().unwrap();
    assert_eq!(maps.len(), 2);
    assert_eq!(maps[0].parent_id, None);
    assert_eq!(maps[1].parent_id, Some(id));
    assert_eq!((maps[0].width, maps[0].height), (100, 50));
}

#[test]
fn open_of_a_read_only_file_gives_a_writable_wal_working_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("world.mmc");
    let store = Store::create(&tmp.path().join("a.sqlite")).unwrap();
    store.save(&file).unwrap();
    store.close().unwrap();
    let mut perm = std::fs::metadata(&file).unwrap().permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(&file, perm).unwrap();

    let wc = tmp.path().join("b.sqlite");
    let store = Store::open(&file, &wc).unwrap();
    store.insert_map(None, "region", 10, 10, 1.0).unwrap();
    let mode: String = Connection::open(&wc)
        .unwrap()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    store.close().unwrap();
}

#[test]
fn close_removes_the_working_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::create(&tmp.path().join("wc.sqlite")).unwrap();
    store.close().unwrap();
    assert!(dir_names(tmp.path()).is_empty());
}

#[test]
fn open_runs_migrations_on_an_older_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("old.mmc");
    {
        let conn = Connection::open(&file).unwrap();
        conn.pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
    }
    let store = Store::open(&file, &tmp.path().join("wc.sqlite")).unwrap();
    assert!(store.maps().unwrap().is_empty());
    let saved = tmp.path().join("new.mmc");
    store.save(&saved).unwrap();
    let conn = Connection::open(&saved).unwrap();
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn entity_outlines_of_an_older_file_become_map_objects() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("old.mmc");
    let before = MIGRATIONS
        .iter()
        .position(|m| m.contains("CREATE TABLE map_objects"))
        .unwrap();
    {
        let conn = Connection::open(&file).unwrap();
        conn.pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        for m in &MIGRATIONS[..before] {
            conn.execute_batch(m).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO maps (kind, width, height, metres_per_pixel) VALUES ('world', 4, 4, 1);
             INSERT INTO entities (id, kind, name, fields) VALUES (7, 'region', 'A', '{}');
             INSERT INTO outlines (map_id, entity_id, points) VALUES (1, 7, x'0102');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", before as i32)
            .unwrap();
    }
    let store = Store::open(&file, &tmp.path().join("wc.sqlite")).unwrap();
    let objects = store.objects(1).unwrap();
    assert_eq!(objects.len(), 1);
    let o = &objects[0];
    assert_eq!(
        (
            o.layer.as_str(),
            o.shape.as_str(),
            o.entity_id,
            o.points.clone()
        ),
        ("outlines", "polygon", Some(7), vec![1, 2])
    );
    assert!(store.next_object_id().unwrap() > o.id);
}

#[test]
fn open_refuses_a_newer_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("new.mmc");
    {
        let conn = Connection::open(&file).unwrap();
        conn.pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
    }
    let wc = tmp.path().join("wc.sqlite");
    let err = Store::open(&file, &wc).err().unwrap();
    assert!(matches!(err, Error::NewerVersion { found, supported }
        if found == SCHEMA_VERSION + 1 && supported == SCHEMA_VERSION));
    assert!(err.to_string().contains("newer"));
    assert!(!wc.exists());
}

#[test]
fn open_refuses_a_foreign_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("other.db");
    Connection::open(&file)
        .unwrap()
        .execute_batch("CREATE TABLE t(x)")
        .unwrap();
    let err = Store::open(&file, &tmp.path().join("wc.sqlite"))
        .err()
        .unwrap();
    assert!(matches!(err, Error::NotAProject));
}

#[test]
fn create_refuses_to_overwrite_a_leftover_working_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let wc = tmp.path().join("wc.sqlite");
    std::fs::write(&wc, b"unsaved").unwrap();
    assert!(matches!(Store::create(&wc), Err(Error::Io(_))));
    assert_eq!(std::fs::read(&wc).unwrap(), b"unsaved");
}

#[test]
fn restore_refuses_a_foreign_working_copy_and_leaves_it() {
    let tmp = tempfile::tempdir().unwrap();
    let wc = tmp.path().join("wc.sqlite");
    std::fs::write(&wc, b"not a database").unwrap();
    assert!(matches!(Store::restore(&wc), Err(Error::NotAProject)));
    assert_eq!(std::fs::read(&wc).unwrap(), b"not a database");
}

#[test]
fn restore_reopens_a_working_copy_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let wc = tmp.path().join("wc.sqlite");
    let store = Store::create(&wc).unwrap();
    store.insert_map(None, "region", 100, 50, 2.5).unwrap();
    drop(store);
    let store = Store::restore(&wc).unwrap();
    assert_eq!(store.maps().unwrap().len(), 1);
    store.close().unwrap();
    Store::discard(&wc).unwrap(); // nothing left is not an error
    assert!(dir_names(tmp.path()).is_empty());
}

#[test]
fn a_live_working_copy_is_locked_against_restore_and_discard() {
    let tmp = tempfile::tempdir().unwrap();
    let wc = tmp.path().join("wc.sqlite");
    let live = Store::create(&wc).unwrap();
    assert!(Store::in_use(&wc));
    assert!(matches!(Store::restore(&wc), Err(Error::InUse)));
    assert!(matches!(Store::discard(&wc), Err(Error::InUse)));
    assert!(wc.exists());
    // A crash releases the lock and leaves the working copy.
    drop(live);
    assert!(!Store::in_use(&wc));
    let restored = Store::restore(&wc).unwrap();
    assert!(Store::in_use(&wc));
    restored.close().unwrap();
    assert!(dir_names(tmp.path()).is_empty());
}
