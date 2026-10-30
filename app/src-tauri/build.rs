//! Build script: generates Tauri's context. Creates an empty `../dist` if the frontend was not built yet, so Rust
//! checks (`cargo clippy --workspace`, `cargo test --workspace`) work without running `npm run build` first. Real
//! builds (`npm run tauri build`) always build the frontend before this runs.

use std::{fs, path::Path};

fn main() {
    let dist = Path::new("../dist");
    if !dist.join("index.html").exists() {
        // A failure here shows up right after as Tauri's own "frontendDist not found" error.
        let _ = fs::create_dir_all(dist);
        let _ = fs::write(dist.join("index.html"), "<!doctype html><title>LanShare</title>");
    }
    tauri_build::build();
}
