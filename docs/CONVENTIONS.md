# Conventions

Binding build, test and code rules. Keep this file under about 8 KB.

## Build

- Cargo workspace, `members = ["crates/*"]`; add a crate as `crates/<name>`, package `mmc-<name>`.
- Crates: `mmc-core` (headless API, seam 1), `mmc-store` (SQLite), `mmc-render` (wgpu, seam 2), `mmc-page`, `mmc-kind-<name>`, `mmc-fixtures` (test and bench data), `mmc-bench` (gate), `mmc-app` (egui).
- Rust stable, edition 2024, Windows x86_64 MSVC. Warnings are errors. Dev profile: dependencies at opt-level 3, workspace at 1.
- Before a push, run `ci.bat` (`ci.bat 2` when another build runs); it runs the CI checks. Keep it and `.github/workflows/ci.yml` in step.
- CI runs on `main` pushes and PRs only, not on docs-only changes. It is a backstop: merge on a green `ci.bat`, not on CI.
- Run the bench gate alone, after `ci.bat`, when a change can affect timings: `cargo run -p mmc-bench --release -- bench-results.json`.

## Architecture

- Only `mmc-store` uses SQL or names SQLite in its `Cargo.toml`.
- Core defines the persistence port (`mmc_core::persist`); `mmc-store` implements it. Callers pass a `Box<dyn Storage>` and the `Kinds` (tests: `mmc_fixtures::create`/`open`/`restore`).
- Core has no UI, no SQL, no global state. Paths the app owns are parameters. `core/src/entities/` never uses maps.
- Errors: one enum per crate, hand-written English `Display`, no error crates. Variants carry data; the app words each through Fluent (`app/src/errors.rs`).
- Comments: rare, one line, objective; state constraints and why.

## Project file

- One SQLite file. `application_id` `0x4D4D4331`; `user_version` = migrations applied. Open refuses another id or a newer version.
- Migrations: append to `MIGRATIONS` in `store/src/lib.rs`, never edit or reorder; feature code and its `MIGRATION` in `store/src/<feature>.rs`. Seed data goes in migrations.
- Open copies the file to a WAL working copy, locked while open; save is `VACUUM INTO <file>.tmp` then rename, on a worker. Nothing else is written next to the project file. Tests that reopen call `wait_for_save` first.
- One edit is one transaction. Tile-only edits go to the edit writer (`core/src/writer.rs`), whose overlay reads see, so an edit never waits for SQLite. Other writes, save, close and `wait_for_jobs` flush it first.
- Worker connections (`Storage::tile_opener`) write at most 1 MiB per transaction and yield to a waiting edit; they alone checkpoint the WAL.
- Deleted maps are flagged until the next open; a new table keyed by map or entity adds its rows to `PURGE` (`store/src/delete.rs`). Entity and object ids are never reused.

## Layers, commands and undo

- `Layer` and `MapKind` are open ids. Core defines `Layer::HEIGHT`, `Layer::LOCK` and `Layer::OUTLINES`; kind crates define the rest.
- Land and sea come from height (ADR-0024); never add a layer named `land`. Coarse land and sea are `WATER_LEVEL ± COAST_RELIEF`. The land test uses `WATER_LEVEL`; the coastline is drawn at `mmc_render::SHORE` (2 m).
- `BrushStroke::target` raises to or lowers to a value, never the other way; `ToolAction::Target` makes such tools.
- Brush maths: `core/src/brush.rs` is the reference (ADR-0025); its fast paths and `detail_tables` give the same bits. Change it and `render/src/live.wgsl` together; `render/tests/live.rs` holds them within 1e-3 m. Relief: `core/src/relief.rs` and `render/src/relief.wgsl` likewise, held within 0.01 m by `render/tests/relief.rs` (ADR-0026). WGSL uses arithmetic or `sqrt`, no trig/`tanh`/`exp` (WARP is off by about 1e-4); shared hashing lives in `render/src/hash.wgsl`.
- Raster tiles: 256 px, key (map, layer, level, tx, ty), f32 LE zstd 3, decoded only by `layer::decode*`. A missing tile is zeros. Level `l` pixels are 2x2 means of `l-1`; only level 0 is edited.
- Write tiles through `Project::write_rows`, one call per transaction: it feeds the cache and the pyramid. Drop a layer with `Write::DropLayer`. Tests use `wait_tiles()`.
- Every edit is a `Command` giving one `undo::Change`; `apply_part` parts are one change until `end_stroke`. Without an outline a stroke stays decoded in memory (`stroke.rs`) until pen up; edits, undo and save write it first. Undo is per map, bounded by `UNDO_LIMIT`; deleting a map goes to its parent's history, a root map to `undo_project`.
- Map objects live on the kind's `object_layers` plus `OUTLINES`. Search: FTS tables in `store/src/search.rs`; the name index lives in `Project`.

## Map kinds

- A kind crate implements `MapKindModule`; a new kind edits neither core nor render.
- A layer is derived when a stage of the map's kind writes it; commands on it fail with `DerivedLayer`.
- Kind options come from `MapKindModule::options()`, are stored per project, and change through `Project::set_kind_option`, which rebuilds derived layers. Stages copy neighbour tiles by row slices, not per-pixel lookups.
- A generator takes a `GenRun` and returns one `Command::Generate`; core records the run with `APP_VERSION`. Runs default to the project seed.
- Pixel centres come from global indices `((origin/mpp).round() + i + 0.5) * mpp`, so neighbours agree. Generated tiles are not edited.
- Children are at most `MAX_CHILD_SIZE` px a side. Regeneration and re-sync: ADR-0022, through a `RegenPlan` and `apply_plan`.
- Every tile write flags the child maps it touches out of date.

## Pipeline and jobs

- Stage work goes through `par_map` (item order), so output never depends on thread count. Long reads check cancel per row.
- Thread counts come from `mmc_core::cpus()` (it honours the process affinity); an edit spawns at most `cpus() - 1`.
- Randomness only through `mmc_core::rng`; `mmc_fixtures::Rng` makes test and bench input, never map content.
- Tests wait through the API (`wait_for_jobs`, `wait_tiles`), never sleep.
- Core workers run below normal priority, their writes (`holding_write_lock`) at normal. `Jobs` runs one job per owner; a submit replaces the waiting one.

## Render

- `mmc-render` uses eframe's wgpu; the app shares its device (`Renderer::from_device`). Windows prefers DX12 (`preferred_backends`).
- Targets are `Rgba8Unorm`. Pass shaders start with `VIEW_WGSL` and use absolute image pixels, so a tiled render equals a single one.
- The screen takes tiles under the project lock (`fetch_cached`), builds the scene after, and redraws until `is_complete()`. Export locks per tile read.
- Image hashes are recorded for WARP. `Renderer::warp()` shares one WARP device per process; never make another.
- Terrain shading lives once in `mmc-render` (`terrain.rs`, `terrain.wgsl`); a kind's height pass calls `terrain(heights, pos.xy)`. Colours change only in `PALETTE`.
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

- `mmc-bench` measures through core's API on `mmc-fixtures` data; results are JSON name to `median_ms`, `budget_ms`, `ok`.
- A budget is one line in `bench/budgets.toml` plus one result; a budget without a result fails, except `*_gpu` without a hardware adapter. Gate on the median of `RUNS`. Budget = the ADR-0014 number, else about 10x the median; raise one only with a reason in the commit.
- GPU budgets time until the frame is drawn: WARP at a small viewport, `*_gpu` at full HD.
- While features are being built, timings are reported, not enforced: the CI gate step does not fail the build, and timing checks in tests print `timing:` warnings instead of failing.

## App

- Logs via `log` to `%LOCALAPPDATA%/MakiMapCreator/logs/`: info for user actions and stroke timings, warn for messages shown and stalls over 50 ms.
- Pen input reaches the app only as `input::PenEvent`; Win32 code stays in `pen_win.rs` behind `cfg(windows)`. Contact is the tip state, not pressure.
- UI strings, units too, live in `app/i18n/en-US/main.ftl` (`tr`, `tr_args`); numbers are formatted first.
- The UI thread only `try_lock`s the project (except open and close) and reads a `Snapshot`; workers use `canvas::lock_project`.
- Edits, undo and redo run in order on the edit worker (`edit.rs`), one stroke part per frame.
- Panels draw and return `Intent`s; `App::apply` applies them. Slow work runs through `App::spawn`.
- Timing harnesses: `cargo test -p mmc-app perf -- --ignored --nocapture`.
