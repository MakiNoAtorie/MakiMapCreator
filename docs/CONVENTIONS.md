# Conventions

Binding build, test and code rules. Keep this file under about 8 KB.

## Build

- Cargo workspace, `members = ["crates/*"]`. Add a crate by creating `crates/<name>`; package name `mmc-<name>`.
- Crates: `mmc-core` (headless public API, seam 1), `mmc-store` (SQLite persistence), `mmc-bench` (benchmark gate), `mmc-app` (egui app).
- Rust stable, edition 2024, target Windows x86_64 MSVC.
- Local checks before a push, the same as CI (`.github/workflows/ci.yml`, windows-latest):
  - `cargo fmt --all --check`
  - `cargo build --workspace --all-targets`
  - `cargo test --workspace`
  - `cargo clippy --workspace --all-targets -- -D warnings`
- Warnings are errors in CI.

## Architecture

- Only `mmc-store` uses SQL or depends on `rusqlite`. CI fails if another crate's `Cargo.toml` names SQLite.
- `mmc-core` depends on `mmc-store` and maps store records to core types. `mmc-store` does not depend on `mmc-core`; its API uses plain records.
- Core has no UI, no SQL and no global state.
- File paths that the app owns (working copy in local app data) are parameters, never looked up inside core or store, so tests use temp dirs.

## Project file

- One SQLite file per project. `PRAGMA application_id` = `0x4D4D4331` ("MMC1"). `PRAGMA user_version` = number of applied migrations.
- Migrations: append to `MIGRATIONS` in `crates/store/src/lib.rs`. Never edit or reorder an existing entry. They run in order, in one transaction, on create and open.
- Store feature code goes in its own module (`crates/store/src/<feature>.rs`) as `impl Store` blocks; its migration is a `pub(crate) const MIGRATION` appended to `MIGRATIONS`.
- Content hashes are BLAKE3, stored as 32-byte BLOBs. Asset blobs are deduplicated by hash.
- Open refuses a file with another `application_id` or a newer `user_version`.
- Open only reads the project file (read-only connection, `VACUUM INTO` the working copy). The working copy uses WAL.
- Save: `VACUUM INTO` `<file>.tmp` in the same folder, then rename over the file. No other file is written next to the project file.
- Close deletes the working copy. A leftover working copy means a crash.

## Layers, commands and undo

- Raster tiles are `TILE_SIZE` (256) px squares; key (map_id, layer, level, tx, ty), level 0 = full resolution. A missing tile row means all zeros, not edited.
- Core owns the tile format (f32 little-endian, zstd level 3). Store keeps tile blobs as opaque bytes.
- Every edit is a `Command`. A command produces before/after records; undo writes the before records, redo the after records, each in one transaction.
- A new raster layer adds a `Layer` variant. Command and undo types never name a layer.
- Undo is per map, in memory, bounded by `UNDO_LIMIT`.
- Every edit returns one `undo::Change`; `write(change, forward)` writes the after or before records.
- Entity ids come from the AUTOINCREMENT sequence and are never reused, so redo stays safe across maps.
- Seed data (kinds, built-in fields) goes in migrations, not code constants.

## Tests

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

## Benchmark gate

- `mmc-bench` measures through the `mmc-core` public API only. CI runs `cargo run -p mmc-bench --release -- bench-results.json` and fails on any exceeded budget. Results are JSON: name to `median_ms`, `budget_ms`, `ok`.
- Add a budget: one line `name = max_ms` in `crates/bench/budgets.toml`, and one entry with the same name in `measure()` in `crates/bench/src/main.rs`. A budget with no result fails.
- Gate on the median of `RUNS` runs, never one run. Budget = the ADR-0014 number; where the ADR gives none, about 10x the measured median. CI runners are noisy: raise a budget only with a reason in the commit.
- Fixtures live in `mmc-fixtures`: seeded (`Rng`), parametric (`LargeMap` size, strokes, radius), shared by tests and bench. Same seed gives the same data.
- Later additions: "many entities" (#10) and "long pages" (#11) generators, with their budgets (name lookup, search).
