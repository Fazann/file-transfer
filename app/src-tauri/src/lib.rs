//! The `LanShare` desktop app: a Tauri shell around `lanshare_core::service::LanShareService`.
//!
//! The window is a Svelte frontend (`app/src`); everything it does goes through the commands in `commands/`, and
//! everything the service reports comes back as events from `events.rs`.

mod commands;
mod diagnostics;
mod error;
mod events;
mod history;
mod settings;
mod state;
mod tray;

use lanshare_core::paths;
use tauri::{Manager, RunEvent, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::state::{lock, AppState};

/// Daily log files to keep.
const LOG_FILES_KEPT: usize = 7;

/// What is logged unless `RUST_LOG` says otherwise.
const DEFAULT_LOG_FILTER: &str = "warn,lanshare_core=info,lanshare_app=info";

/// Logs to stderr and to a daily file in `paths::log_dir()`. Without a usable log folder, logs only go to stderr.
/// Lines still waiting to reach the file are written when the returned guard is dropped.
fn init_logging() -> Option<WorkerGuard> {
    let appender = paths::log_dir().ok().and_then(|dir| {
        RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix(diagnostics::LOG_FILE_PREFIX)
            .filename_suffix("log")
            .max_log_files(LOG_FILES_KEPT)
            .build(dir)
            .ok()
    });
    let (writer, guard) = appender.map(tracing_appender::non_blocking).unzip();
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER)))
        .with(fmt::layer())
        .with(writer.map(|writer| fmt::layer().with_ansi(false).with_writer(writer)))
        .init();
    guard
}

/// Starts the app and runs until it quits.
///
/// # Errors
///
/// When the window or the service cannot start.
pub fn run() -> tauri::Result<()> {
    let _log_guard = init_logging();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let handle = app.handle().clone();
            // Setup is not async; the service needs the async runtime that Tauri already runs.
            let state = tauri::async_runtime::block_on(AppState::init(&handle))
                .map_err(|error| format!("LanShare could not start: {}", error.code))?;
            app.manage(state);
            tray::create(&handle)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let close_to_tray =
                    window.try_state::<AppState>().is_some_and(|state| lock(&state.settings).close_to_tray);
                if close_to_tray {
                    // Hide instead of closing; receiving continues and the tray brings the window back.
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::devices::get_self,
            commands::devices::list_peers,
            commands::transfers::send_files,
            commands::transfers::respond_to_offer,
            commands::transfers::pause,
            commands::transfers::resume,
            commands::transfers::cancel,
            commands::transfers::pause_all,
            commands::pairing::start_pairing,
            commands::pairing::submit_pin,
            commands::pairing::list_trusted,
            commands::pairing::remove_trusted,
            commands::pairing::set_auto_accept,
            commands::settings::get_settings,
            commands::settings::update_settings,
            commands::settings::set_tray_labels,
            commands::settings::export_diagnostics,
            commands::history::get_history,
            commands::history::clear_history,
            commands::history::open_folder,
        ])
        .build(tauri::generate_context!())?;
    app.run(|handle, event| match event {
        // macOS: clicking the Dock icon brings a hidden window back.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => tray::show_window(handle),
        RunEvent::ExitRequested { .. } => {
            if let Some(state) = handle.try_state::<AppState>() {
                // Say goodbye on the network so other devices drop this one at once.
                tauri::async_runtime::block_on(async { state.service().await.shutdown().await });
            }
        }
        _ => {}
    });
    Ok(())
}
