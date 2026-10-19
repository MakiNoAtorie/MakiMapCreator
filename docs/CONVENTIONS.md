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

## Tests

- Test external behaviour through public seams only. Seam 1 tests live in `crates/core/tests/` and use only the `mmc-core` API.
- Store tests (`crates/store/tests/`) may use `rusqlite` to build fixtures and read pragmas.
- Fixtures are generated in the test, never committed binaries. Use `tempfile::tempdir()` for every file.

## Code

- Errors: one `Error` enum per crate with a hand-written `Display`; no error crates.
- Comments: rare, one line, objective. State constraints and why.
