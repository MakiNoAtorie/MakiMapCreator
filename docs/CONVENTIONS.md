# Conventions

Binding build, test and code rules. Keep this file under about 8 KB.

## Build

- Workspace `members = ["crates/*"]`: a crate is `crates/<name>`, package `mmc-<name>`.
- Before a push, run `ci.bat` (`ci.bat 2` when another build runs); keep it and `.github/workflows/ci.yml` in step. CI is a backstop: merge on a green `ci.bat`.
- When a change can affect timings, run the bench gate alone after `ci.bat`: `cargo run -p mmc-bench --release -- bench-results.json`.

## Architecture

- Only `mmc-store` uses SQL or names SQLite in its `Cargo.toml`.
- Core defines the persistence port (`mmc_core::persist`); `mmc-store` implements it. Callers pass a `Box<dyn Storage>` and the `Kinds` (tests: `mmc_fixtures::create`/`open`/`restore`).
- Core has no UI, no SQL, no global state; paths the app owns are parameters. `core/src/entities/` never uses maps.
- Errors: one enum per crate, hand-written English `Display`, no error crates. Variants carry data; the app words each through Fluent (`app/src/errors.rs`).
- Comments: rare, one line, objective; state constraints and why.

## Project file

- One SQLite file; `user_version` = migrations applied.
- Migrations: append to `MIGRATIONS` (`store/src/lib.rs`), never edit or reorder; a feature's code and `MIGRATION` live in `store/src/<feature>.rs`. Seed data goes in migrations.
- Open copies the file to a locked WAL working copy; save is `VACUUM INTO <file>.tmp` then rename, on a worker. Nothing else is written next to the file. Tests that reopen call `wait_for_save` first.
- One edit is one transaction. Tile-only edits go to the edit writer (`core/src/writer.rs`), whose overlay reads see, so an edit never waits for SQLite; other writes, save, close and `wait_for_jobs` flush it first.
- Worker connections (`Storage::tile_opener`) write at most 1 MiB per transaction, yield to a waiting edit, and alone checkpoint the WAL.
- Deleted maps are flagged until the next open; a new table keyed by map or entity adds its rows to `PURGE` (`store/src/delete.rs`). Entity and object ids are never reused.

## Layers, commands and undo

- `Layer` and `MapKind` are open ids. Core defines `Layer::HEIGHT`, `LOCK` and `OUTLINES`; kind crates the rest.
- Land and sea come from height (ADR-0024); no layer named `land`. Coarse land and sea are `WATER_LEVEL ± COAST_RELIEF`; the land test uses `WATER_LEVEL`; the coastline is drawn at `mmc_render::SHORE` (2 m).
- `BrushStroke::target` raises to or lowers to a value, never the other way; `ToolAction::Target` makes such tools.
- Brush maths: `core/src/brush.rs` is the reference (ADR-0025); its fast paths and `detail_tables` give the same bits. Change it with `render/src/live.wgsl`; `render/tests/live.rs` holds them within 1e-3 m. Relief likewise: `core/src/relief.rs` with `render/src/relief.wgsl`, within 0.01 m by `render/tests/relief.rs` (ADR-0026). WGSL uses arithmetic or `sqrt`, no trig/`tanh`/`exp` (WARP is off by about 1e-4); shared hashing is in `render/src/hash.wgsl`. Avoid `x^p`, p < 1, near 0: it breaks f32 parity.
- Raster tiles: 256 px, key (map, layer, level, tx, ty), f32 LE zstd 3, decoded only by `layer::decode*`. A missing tile is zeros. Level `l` pixels are 2x2 means of `l-1`; only level 0 is edited.
- Write tiles through `Project::write_rows`, one call per transaction (it feeds the cache and pyramid). Drop a layer with `Write::DropLayer`.
- Every edit is a `Command` giving one `undo::Change`; `apply_part` parts are one change until `end_stroke`. Without an outline a stroke stays in memory (`stroke.rs`) until pen up; edits, undo and save write it first. Undo is per map, bounded by `UNDO_LIMIT`; deleting a map goes to its parent's history, a root map to `undo_project`.
- Map objects live on the kind's `object_layers` plus `OUTLINES`. Search: FTS tables in `store/src/search.rs`; the name index lives in `Project`.

## Map kinds

- A kind crate implements `MapKindModule`; a new kind edits neither core nor render.
- A layer is derived when a stage of the map's kind writes it; commands on it fail with `DerivedLayer`.
- Kind options come from `MapKindModule::options()`, are stored per project, and change through `Project::set_kind_option`, which rebuilds derived layers. Stages copy neighbour tiles by row slices, not per-pixel lookups. A stage that reads past the map edge sets `Stage::reach` and reads through `StageInput::padded`, never clamping itself; only child maps of one tree, kind and scale are neighbours.
- A generator takes a `GenRun` and returns one `Command::Generate`; core records the run with `APP_VERSION`. Runs default to the project seed.
- Pixel centres come from global indices `((origin/mpp).round() + i + 0.5) * mpp`, so neighbours agree. Generated tiles are not edited.
- Children are at most `MAX_CHILD_SIZE` px a side. Regeneration and re-sync: ADR-0022, via `RegenPlan` and `apply_plan`. Every tile write flags the child maps it touches out of date.

## Pipeline and jobs

- Stage work goes through `par_map` (item order), so output never depends on thread count. Long reads check cancel per row.
- Thread counts come from `mmc_core::cpus()`; an edit spawns at most `cpus() - 1`.
- Randomness only through `mmc_core::rng`; `mmc_fixtures::Rng` makes test and bench input, never map content.
- Tests wait through the API (`wait_for_jobs`, `wait_tiles`), never sleep.
- Core workers run below normal priority, their writes (`holding_write_lock`) at normal. `Jobs` runs one job per owner; a submit replaces the waiting one.

## Render

- `mmc-render` uses eframe's wgpu; the app shares its device (`Renderer::from_device`).
- Targets are `Rgba8Unorm`. Pass shaders start with `VIEW_WGSL` and use absolute image pixels.
- The screen takes tiles under the project lock (`fetch_cached`), builds the scene after, and redraws until `is_complete()`. Export locks per tile read.
- Image hashes are recorded for WARP. `Renderer::warp()` shares one WARP device per process; never make another.
- Terrain shading lives once in `terrain.rs`/`terrain.wgsl`; a kind's height pass calls `terrain(heights, pos.xy)`. Colours change only in `PALETTE`.
- GPU structs are `#[repr(C)]` bytemuck structs, tested against their WGSL layout (naga).
- Live brush: ADR-0025. `live::Applied` is written and read under the project lock.

## Pages and anchors

- `mmc-page` depends on neither core nor store; ported code keeps its MIT notice.
- Links are stored as `[[id|label]]`; labels are never rewritten. Core parses links; the link index is written with the page.
- Anchor edits are commands; matching follows derived data and is not undoable. A stage that finds features calls `set_derived_features` before it commits.

## Tests

- Test behaviour through public seams. Core tests use only the `mmc-core` API; store tests may use `rusqlite`.
- The proof slice runs in `kind-region/tests/proof_slice.rs`, step 9 in `render/tests/render.rs`; a proof-relevant feature extends it.
- Fixtures are generated in `tempfile::tempdir()`. Timing asserts loosen 10x in debug builds.

## Benchmark gate

- `mmc-bench` measures through core's API on `mmc-fixtures` data.
- A budget is one line in `bench/budgets.toml` plus one result; a budget without a result fails, except `*_gpu` without a hardware adapter. Gate on the median of `RUNS`. Budget = the ADR-0014 number, else about 10x the median; raise one only with a reason in the commit.
- While features are being built, timings are reported, not enforced: the CI gate does not fail the build, and test timing checks print `timing:` warnings.

## App

- Logs via `log` to `%LOCALAPPDATA%/MakiMapCreator/logs/`: info for user actions and stroke timings, warn for messages shown and stalls over 50 ms.
- Pen input reaches the app only as `input::PenEvent`; Win32 code stays in `pen_win.rs` behind `cfg(windows)`. Contact is the tip state, not pressure.
- UI strings, units too, live in `app/i18n/en-US/main.ftl` (`tr`, `tr_args`); numbers are formatted first.
- The UI thread only `try_lock`s the project (except open and close) and reads a `Snapshot`; workers use `canvas::lock_project`.
- Edits, undo and redo run in order on the edit worker (`edit.rs`), one stroke part per frame.
- Panels draw and return `Intent`s; `App::apply` applies them. Slow work runs through `App::spawn`.
