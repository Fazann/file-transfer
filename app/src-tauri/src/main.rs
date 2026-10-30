//! Entry point of the desktop app.

// Without this, Windows opens a console window next to the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> tauri::Result<()> {
    lanshare_app::run()
}
