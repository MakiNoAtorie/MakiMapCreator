//! Embeds the font files present in `fonts/` (see `theme::FONT_FILES`); absent files are skipped.

use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fonts");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut out = String::from("&[\n");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "ttf" || x == "otf") {
                let name = p.file_name().unwrap().to_string_lossy();
                out += &format!("    ({name:?}, include_bytes!({:?}).as_slice()),\n", p);
            }
        }
    }
    out += "]\n";
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("fonts.rs");
    std::fs::write(dest, out).unwrap();
}
