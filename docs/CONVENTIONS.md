# Conventions

Binding build, test and code rules. Keep this file under about 8 KB.

## Build

- Cargo workspace, `members = ["crates/*"]`. Add a crate by creating `crates/<name>`; package name `mmc-<name>`.
- Crates: `mmc-core` (headless public API, seam 1), `mmc-store` (SQLite persistence), `mmc-bench` (benchmark gate), `mmc-fixtures` (seeded test and bench data), `mmc-render` (wgpu renderer, seam 2), `mmc-page` (page editing core), `mmc-kind-<name>` (map kinds), `mmc-app` (egui app).
- Rust stable, edition 2024, target Windows x86_64 MSVC.
- Local checks before a push, the same as CI (`.github/workflows/ci.yml`, windows-latest):
  - `cargo fmt --all --check`
  - `cargo build --workspace --all-targets`
  - `cargo test --workspace`
  - `cargo clippy --workspace --all-targets -- -D warnings`
- Warnings are errors in CI.

## Architecture

- Only `mmc-store` uses SQL or depends on `rusqlite`. CI fails if another crate's `Cargo.toml` names SQLite.
- `mmc-core` defines the persistence port (`mmc_core::persist`: `Storage`, `TileStore`, `TextIndex`, `ProjectReader` and the records). `mmc-store` implements it and depends on core; core never names the store. App, tests and bench pass a `Box<dyn Storage>` in (tests: `mmc_fixtures::create`/`open`/`restore`).
- One edit is one `Storage::write` batch of `persist::Write` rows: one transaction.
- Core has no UI, no SQL and no global state.
- File paths that the app owns (working copy in local app data) are parameters, never looked up inside core or store, so tests use temp dirs.

## Project file

- One SQLite file per project. `PRAGMA application_id` = `0x4D4D4331` ("MMC1"). `PRAGMA user_version` = number of applied migrations.
- Migrations: append to `MIGRATIONS` in `crates/store/src/lib.rs`. Never edit or reorder an existing entry. They run in order, in one transaction, on create and open.
- Store feature code goes in its own module (`crates/store/src/<feature>.rs`) as `impl Store` blocks; its migration is a `pub(crate) const MIGRATION` appended to `MIGRATIONS`.
- Content hashes are BLAKE3, stored as 32-byte BLOBs. Asset blobs are deduplicated by hash.
- Open refuses a file with another `application_id` or a newer `user_version`.
- Open copies the project file to the working copy as a plain byte copy under a read lock (no `VACUUM INTO`, no fsync on open). The working copy uses WAL.
- Save: `VACUUM INTO` `<file>.tmp` in the same folder, then rename over the file. No other file is written next to the project file.
- Close deletes the working copy. A leftover working copy means a crash.
- Save runs on a worker: `Project::save` returns at once; use `poll_save`/`wait_for_save`. Tests that open the file before `close` call `wait_for_save` first. Save reads one WAL snapshot on its own connection and never takes the write lock.
- The working copy uses `PRAGMA synchronous = NORMAL` (WAL): an app crash loses nothing; a power loss can lose the last edits.
- Long reads in jobs check cancel per row (`TileConn::layer_tiles_until`).
- Restore opens a leftover working copy in place; discard deletes it and its WAL files.

## Layers, commands and undo

- Raster tiles are `TILE_SIZE` (256) px squares; key (map_id, layer, level, tx, ty), level 0 = full resolution. A missing tile row means all zeros, not edited.
- Core owns the tile format (f32 little-endian, zstd level 3). Store keeps tile blobs as opaque bytes.
- Every edit is a `Command`. A command produces before/after records; undo writes the before records, redo the after records, each in one transaction.
- `Layer` and `MapKind` are open ids (`Layer::new("water")`). Core defines only `Layer::HEIGHT` and `Layer::LOCK`; a kind crate defines its own layers and kind id. Command and undo types never name a layer.
- Undo is per map, in memory, bounded by `UNDO_LIMIT`.
- Deleting map M goes to the history of M's parent; for a root map, to the project history (`undo_project`, key `MapId(0)`). A `Change` variant that deletes maps is handled in `delete::history_key`.
- Deleted maps are flagged, not removed, until the project is next opened. A new table keyed by map or entity adds its rows to `PURGE` in `store/src/delete.rs`.
- Every edit returns one `undo::Change`; `write(change, forward)` writes the after or before records.
- Entity and map object ids come from AUTOINCREMENT sequences and are never reused, so redo stays safe across maps.
- Map objects (point, path, polygon; optional entity and label offset) live on object layers: the kind's `object_layers` plus `Layer::OUTLINES`. Entity outlines are polygons on `Layer::OUTLINES`; edits are `Command::Object`.
- Seed data (kinds, built-in fields) goes in migrations, not code constants.
- Search: FTS tables live in `store/src/search.rs` (rowid = entity id), kept in the same transaction as entity and page writes. The in-memory name index lives in `Project` and is updated in `write_entity_changes`.
- Write raster tiles through core (`Project::write_rows`), never `Storage::write` directly: it updates the pyramid and the tile cache.
- Pyramid level `l` pixel = 2x2 mean of level `l-1`; the top level (one tile) is the map's overview. Pyramid tiles have `edited = false`; `edited_tiles` is level 0 only.
- Edits write level 0; a background worker updates upper levels. `Project::save` waits for it before the snapshot. Tests use `wait_tiles()`.
- The screen uses `scene_cached` and redraws until `is_complete()`; export uses `scene` (level 0, blocking).

## Tests

- The whole proof slice (steps 1, 2, 4–8) runs in `crates/kind-region/tests/proof_slice.rs`; step 9 in `crates/render/tests/render.rs`. A new proof-relevant feature extends the slice.
- Timing asserts in tests are loosened 10x in debug builds; the release gate holds the real budgets.

- Test external behaviour through public seams only. Seam 1 tests live in `crates/core/tests/` and use only the `mmc-core` API.
- Store tests (`crates/store/tests/`) may use `rusqlite` to build fixtures and read pragmas.
- Fixtures are generated in the test, never committed binaries. Use `tempfile::tempdir()` for every file.

## Code

- Errors: one `Error` enum per crate with a hand-written `Display`; no error crates.
- Comments: rare, one line, objective. State constraints and why.

## App

- `mmc-app` uses eframe on wgpu (ADR-0013). Pen input reaches the app only as `input::PenEvent`; Win32 code stays in `pen_win.rs` behind `cfg(windows)`.
- `PenEvent.pos` is in window client physical pixels; divide by `pixels_per_point` for egui points.
- Win32 calls use the `windows-sys` version that eframe uses.
- UI strings live in `crates/app/i18n/en-US/main.ftl`, read through `i18n::tr`/`tr_args`. Numbers are formatted in Rust before they go in as arguments.
- Fonts go in `crates/app/fonts/` with the names in `theme::FONT_FILES`; missing files fall back to egui defaults.
- Project file extension `.mmc`; working copies in `%LOCALAPPDATA%/MakiMapCreator/working/`.
- The app holds the project in `Arc<Mutex<Option<Project>>>`; the UI thread only uses `try_lock`, except on open and close.

## Benchmark gate

- `mmc-bench` measures through the `mmc-core` public API only. CI runs `cargo run -p mmc-bench --release -- bench-results.json` and fails on any exceeded budget. Results are JSON: name to `median_ms`, `budget_ms`, `ok`.
- Add a budget: one line `name = max_ms` in `crates/bench/budgets.toml`, and one entry with the same name in `measure()` in `crates/bench/src/main.rs`. A budget with no result fails.
- Gate on the median of `RUNS` runs, never one run. Budget = the ADR-0014 number; where the ADR gives none, about 10x the measured median. CI runners are noisy: raise a budget only with a reason in the commit.
- Fixtures live in `mmc-fixtures`: seeded (`Rng`), parametric (`LargeMap` size, strokes, radius), shared by tests and bench. Same seed gives the same data.
- Generators: large map, many entities, long pages (`mmc-fixtures`). Name lookup, link completion and command search share the `name_lookup` budget (`find_names`).
- App structure: `workspace.rs` holds testable logic; `panels.rs` draws and returns `Intent`s; `App::apply` applies them. The UI reads a `Snapshot` reloaded under `try_lock` when dirty, never the project directly.
- Slow app work (generate, child creation) runs through `App::spawn` on a worker. Commands applied outside the brush call `note_command` so undo groups stay in step.

## Map kinds

- A kind crate is `crates/kind-<name>` implementing `MapKindModule`: default settings, layers, stages (derived layers), render passes (WGSL text, no wgpu), tools, generators with `Param`s, child decl (kind, scale factor, context layers), summary, entity kinds. The caller builds `Kinds` and passes it to `Project::create`/`load`. A new kind edits neither core nor render. No runtime plugins.
- A layer is derived when a stage of the map's kind writes it (`Project::is_derived`).
- A generator takes a `GenRun` (generator, seed, params) and returns one `Command::Generate`; core assigns entity ids and records the run and `APP_VERSION`.
- A store write that also writes entities calls `write_entity_rows` inside its own transaction; a core path that creates entities updates `names`.
- Each map has a root-space `origin` (root maps at 0,0) on its own pixel grid. Generators compute pixel centres from global indices: `((origin/mpp).round() + i + 0.5) * mpp`, so neighbours agree.
- Generated tiles have `edited = false`. Child-map store code lives in `store/src/children.rs`.
- A kind's seam tests go in `crates/kind-<name>/tests/`; dev-dependencies include the parent kind.
- Out-of-date marking runs in `Project::write`, so every tile write flags the child maps it touches.
- Regeneration and re-sync are preview (a `RegenPlan`, no writes) then `apply_plan` (one command). A plan whose `before` records changed is refused with `StalePlan`. Edited tiles and locked pixels (`Layer::Lock` > 0.5) are never overwritten.

## Pipeline and jobs

- A derived layer is one a `STAGES` entry in `core/src/pipeline.rs` writes; commands on it return `Error::DerivedLayer`.
- Rebuilds read and write on their own connection (`TileConn`); a commit replaces whole layers in one transaction.
- Stage work goes through `par_map` (results in item order), so output does not depend on thread count.
- Randomness only through `mmc_core::rng` (stateless hash), never stateful RNGs.
- Tests wait on jobs through the API (`wait_for_jobs`), never sleep.

## Render

- `mmc-render` uses the wgpu version eframe uses; the app shares one device through `Renderer::from_device`.
- Render targets are `Rgba8Unorm` (not sRGB), so image bytes are exactly what the shader writes.
- Each pass shader starts with `VIEW_WGSL` and computes map texels from absolute image pixels, so a tiled render equals a single render.
- Image hashes are per backend. Recorded hashes are for WARP; render tests use `Renderer::warp()`.

## Pages

- `mmc-page` depends on neither `mmc-core` nor `mmc-store`. Ported code keeps its MIT notice at the top of the crate.
- Page links are stored only as `[[id|label]]` in page text; the label is never rewritten on rename.
- Core parses links; store never parses Markdown. The link index is written in the same transaction as the page.

## Anchors

- Anchor edits are commands (`Change::Anchor`); anchor matching is not undoable, because it follows derived data.
- A derived stage that outputs features calls `set_derived_features` before its rebuild commits.
- River geometry runs source to mouth; lake geometry is a polygon ring.
