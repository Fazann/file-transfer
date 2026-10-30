//! The tray icon with Open, Pause all and Quit. Labels start in English and are replaced by the frontend's
//! translations through `set_tray_labels`.

use serde::Deserialize;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, Wry,
};

use crate::state::AppState;

/// Translated tray labels from the frontend.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayLabels {
    /// Shows the window.
    pub open: String,
    /// Pauses every transfer.
    pub pause_all: String,
    /// Quits the app.
    pub quit: String,
}

/// The tray menu items, kept in managed state to change their labels.
pub struct TrayMenu {
    open: MenuItem<Wry>,
    pause_all: MenuItem<Wry>,
    quit: MenuItem<Wry>,
}

impl TrayMenu {
    /// Replaces the labels.
    ///
    /// # Errors
    ///
    /// When the operating system rejects the change.
    pub fn set_labels(&self, labels: &TrayLabels) -> tauri::Result<()> {
        self.open.set_text(&labels.open)?;
        self.pause_all.set_text(&labels.pause_all)?;
        self.quit.set_text(&labels.quit)
    }
}

/// Creates the tray icon and puts its menu in managed state.
///
/// # Errors
///
/// When the tray cannot be created.
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open LanShare", true, None::<&str>)?;
    let pause_all = MenuItem::with_id(app, "pause_all", "Pause all", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &pause_all, &quit])?;
    let mut builder = TrayIconBuilder::with_id("main").menu(&menu).tooltip("LanShare").on_menu_event(|app, event| {
        match event.id.as_ref() {
            "open" => show_window(app),
            "pause_all" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(state) = app.try_state::<AppState>() {
                        state.service().await.pause_all();
                    }
                });
            }
            "quit" => app.exit(0),
            _ => {}
        }
    });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    app.manage(TrayMenu { open, pause_all, quit });
    Ok(())
}

/// Shows and focuses the main window, for example after it was hidden to the tray.
pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
