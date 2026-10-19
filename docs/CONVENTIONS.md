# Conventions

Binding build, test and code rules. Keep this file under about 8 KB.

## Build

- Cargo workspace, `members = ["crates/*"]`; add a crate as `crates/<name>`, package `mmc-<name>`.
- Crates: `mmc-core` (headless API, seam 1), `mmc-store` (SQLite), `mmc-render` (wgpu, seam 2), `mmc-page`, `mmc-kind-<name>`, `mmc-fixtures` (seeded test and bench data), `mmc-bench` (gate), `mmc-app` (egui).
- Rust stable, edition 2024, Windows x86_64 MSVC. Warnings are errors in CI.
- Dev profile: dependencies at opt-level 3, workspace at 1 (the owner runs debug builds).
- Before a push, as CI: `cargo fmt --all --check`, `cargo build --workspace --all-targets`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and the release gate `cargo run -p mmc-bench --release -- bench-results.json`.

## Architecture

- Only `mmc-store` uses SQL or names SQLite in its `Cargo.toml`.
- Core defines the persistence port (`mmc_core::persist`); `mmc-store` implements it. Callers pass a `Box<dyn Storage>` and the `Kinds` (tests: `mmc_fixtures::create`/`open`/`restore`).
- Core has no UI, no SQL, no global state. Paths the app owns are parameters.
- `core/src/entities/` (entities, fields, relations, pages, search, anchors) uses only core types and the port, never maps.
- Errors: one enum per crate, hand-written English `Display` for logs, no error crates. Variants carry data; the app words each in `app/src/errors.rs` through Fluent.
- Comments: rare, one line, objective; state constraints and why.

## Project file

- One SQLite file. `application_id` `0x4D4D4331`; `user_version` = migrations applied. Open refuses another id or a newer version.
- Migrations: append to `MIGRATIONS` in `store/src/lib.rs`, never edit or reorder. Store feature code lives in `store/src/<feature>.rs` with its `MIGRATION`. Seed data goes in migrations.
- Open copies the file to the working copy (plain copy under a read lock); the working copy uses WAL, `synchronous = NORMAL`.
- Save runs on a worker (`save`, `poll_save`, `wait_for_save`): `VACUUM INTO <file>.tmp`, then rename. Nothing else is written next to the project file. Tests that reopen call `wait_for_save` first.
- An open store holds an OS lock on `<working copy>-lock`; close deletes the working copy. Restore, discard and the launch prompt skip a locked (live) working copy.
- One edit is one `Storage::write` batch of `persist::Write` rows, one transaction.
- Edits use `BEGIN IMMEDIATE` and a 100 µs busy handler. Worker connections (`Storage::tile_opener`) write about 1 MiB per transaction, wait while an edit is pending, and alone checkpoint the WAL.
- Deleted maps are flagged until the next open; a new table keyed by map or entity adds its rows to `PURGE` in `store/src/delete.rs`. Ids of entities and map objects come from AUTOINCREMENT and are never reused.

## Layers, commands and undo

- `Layer` and `MapKind` are open ids. Core defines `Layer::HEIGHT`, `Layer::LOCK` and the object layer `Layer::OUTLINES`; kind crates define the rest.
- Raster tiles: 256 px, key (map, layer, level, tx, ty), f32 LE zstd 3; core owns the format. A missing tile is zeros, not edited. Level `l` pixels are 2x2 means of `l-1`; the top tile is the overview; only level 0 is ever edited.
- Write tiles through `Project::write_rows`: it feeds the cache and the pyramid worker. Tests use `wait_tiles()`.
- Every edit is a `Command` giving one `undo::Change` with before and after records; `apply_part` parts merge into one until `end_stroke`, which starts the derived rebuild. Undo is per map, in memory, bounded by `UNDO_LIMIT`. Deleting map M goes to M's parent history, a root map to `undo_project`; child map creation to the parent history.
- Map objects (point, path, polygon) live on the kind's `object_layers` plus `OUTLINES` (entity outlines).
- Search: FTS tables in `store/src/search.rs`, written with entity and page rows; the name index lives in `Project`.

## Map kinds

- A kind crate implements `MapKindModule`: default settings, layers, stages, render passes (WGSL text), tools, generators with `Param`s, child decl (kind, scale factor, context layers), summary, entity kinds. A new kind edits neither core nor render.
- A layer is derived when a stage of the map's kind writes it; commands on it fail with `DerivedLayer`.
- A generator takes a `GenRun` (generator, seed, params) and returns one `Command::Generate`; core assigns ids and records the run with `APP_VERSION`. Runs default to the project seed.
- Maps have a root-space `origin`; generators compute pixel centres from global indices `((origin/mpp).round() + i + 0.5) * mpp`, so neighbours agree. Generated tiles are not edited.
- Children are at most `MAX_CHILD_SIZE` px a side; a default scale doubles until the child fits.
- Regeneration and re-sync preview a `RegenPlan`, then `apply_plan` writes one command; a changed map gives `StalePlan`. Edited tiles and locked pixels (`LOCK` > 0.5) are kept. Lost ground keeps its data until confirmed, then clears to 0. An outline past the map grows it by whole tiles (`Write::ShiftTiles`).
- Every tile write flags the child maps it touches out of date.

## Pipeline and jobs

- Stage work goes through `par_map` (item order), so output never depends on thread count. Long reads check cancel per row.
- Randomness only through `mmc_core::rng` (stateless hash). Exception: `mmc_fixtures::Rng` makes seeded test and bench input, never map content.
- Deleting a map stops its jobs; undo of the deletion rebuilds it.
- Tests wait through the API (`wait_for_jobs`, `wait_tiles`), never sleep.

## Render

- `mmc-render` uses eframe's wgpu; the app shares its device through `Renderer::from_device`.
- Targets are `Rgba8Unorm`. Pass shaders start with `VIEW_WGSL` and use absolute image pixels, so a tiled render equals a single render.
- The screen takes tiles under the project lock (`fetch_cached`), builds the scene after, and redraws until `is_complete()` (pyramid settled too). Export renders bands, locking per tile read. Windows prefers DX12 (`preferred_backends`).
- Image hashes are per backend, recorded for WARP. `Renderer::warp()` shares one WARP device per process; never make another.

## Pages and anchors

- `mmc-page` depends on neither core nor store; ported code keeps its MIT notice.
- Links are stored as `[[id|label]]`; labels are never rewritten. Link markup in code is text. Core parses links; the link index is written with the page.
- Anchor edits are commands; matching follows derived data and is not undoable. A stage that finds features calls `set_derived_features` before it commits.

## Tests

- Test behaviour through public seams. Core tests use only the `mmc-core` API; store tests may use `rusqlite`.
- The proof slice (steps 1, 2, 4–8) runs in `kind-region/tests/proof_slice.rs`; step 9 in `render/tests/render.rs`. A proof-relevant feature extends it.
- Fixtures are generated in the test, in `tempfile::tempdir()`. Timing asserts loosen 10x in debug builds.

## Benchmark gate

- `mmc-bench` measures through core's API on `mmc-fixtures` data and fails on any exceeded budget; results are JSON name to `median_ms`, `budget_ms`, `ok`.
- A budget is one line in `bench/budgets.toml` plus one entry in `measure()`; a budget with no result fails. Gate on the median of `RUNS`. Budget = the ADR-0014 number, else about 10x the median; raise one only with a reason in the commit.

## App

- The app logs to `%LOCALAPPDATA%/MakiMapCreator/logs/mmc.log` (previous: `mmc.prev.log`) via `log`: info for user actions and results and per-stroke timings, warn for messages shown and stalls over 50 ms; dependencies log warnings only.
- eframe on wgpu (ADR-0013). Pen input reaches the app only as `input::PenEvent` (client physical pixels); Win32 code stays in `pen_win.rs` behind `cfg(windows)`, on eframe's `windows-sys`.
- WinTab: large queue, drained with `WTPacketsGet`. Contact is the tip state (Ink: `POINTER_FLAG_INCONTACT`), not pressure.
- The pen API is a setting in `%LOCALAPPDATA%/MakiMapCreator/settings.txt` (`pen = wintab|ink`), read at start; a first argument overrides it.
- UI strings live in `app/i18n/en-US/main.ftl` (`tr`, `tr_args`); numbers are formatted before they go in. Fonts in `app/fonts/` (`theme::FONT_FILES`).
- The project is in `Arc<Mutex<Option<Project>>>`; the UI thread only uses `try_lock` (except open and close) and reads a `Snapshot`; workers lock with `canvas::lock_project` (logs waits over 50 ms).
- Edits, undo and redo run in order on the edit worker (`edit.rs`). A stroke goes as one part per frame; a part's overlay stays until a render requested after it was applied lands.
- `main.rs` holds `App` and the frame; `impl App` blocks by concern live in their own files. Panels draw and return `Intent`s; `App::apply` applies them.
- Slow work runs through `App::spawn`; export on its own thread. Generate lists only generators that `runs_on` the shown map.
- Timing harnesses: `cargo test -p mmc-app perf -- --ignored --nocapture`.
