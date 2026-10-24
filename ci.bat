@echo off
rem Runs the CI checks locally. Pass the cargo job count (default 4; use 2 when another build runs).
cd /d "%~dp0"
set J=%1
if "%J%"=="" set J=4
cargo fmt --all --check || exit /b 1
cargo build -j %J% --workspace --all-targets || exit /b 1
cargo test -j %J% --workspace -- --test-threads=2 || exit /b 1
cargo clippy -j %J% --workspace --all-targets -- -D warnings || exit /b 1
git grep -q -i -E "rusqlite|sqlite" -- "crates/*/Cargo.toml" ":!crates/store" && (echo SQL dependency outside crates/store & exit /b 1)
echo CI checks passed.
