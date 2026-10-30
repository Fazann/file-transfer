//! Settings (network ones in `config.toml`, app ones in `app-settings.json`) and tray labels.

use std::path::PathBuf;

use lanshare_core::paths;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State, Theme};
use tauri_plugin_autostart::ManagerExt;

use crate::{
    diagnostics,
    error::{CommandError, CommandResult},
    settings::AppSettings,
    state::{lock, AppState},
    tray::{TrayLabels, TrayMenu},
};

/// Everything the Settings page edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPayload {
    device_name: String,
    download_dir: String,
    port: u16,
    streams: usize,
    bandwidth_limit_mbps: u32,
    allow_untrusted: bool,
    language: Option<String>,
    theme: Option<Theme>,
    close_to_tray: bool,
    start_at_login: bool,
}

/// The current settings.
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> SettingsPayload {
    let config = lock(&state.config).clone();
    let settings = lock(&state.settings).clone();
    SettingsPayload {
        device_name: config.device_name,
        download_dir: config.download_dir.display().to_string(),
        port: config.port,
        streams: config.streams,
        bandwidth_limit_mbps: config.bandwidth_limit_mbps,
        allow_untrusted: config.allow_untrusted,
        language: settings.language,
        theme: settings.theme,
        close_to_tray: settings.close_to_tray,
        start_at_login: settings.start_at_login,
    }
}

/// Saves the settings and applies them. A new device name, port or download folder restarts the receiver, which
/// cancels running transfers. Returns the settings as saved (values out of range are corrected).
#[tauri::command]
pub async fn update_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: SettingsPayload,
) -> CommandResult<SettingsPayload> {
    let old = lock(&state.config).clone();
    let mut config = old.clone();
    config.device_name = settings.device_name.trim().to_owned();
    config.download_dir = PathBuf::from(&settings.download_dir);
    config.port = settings.port;
    config.streams = settings.streams.clamp(1, lanshare_core::transfer::MAX_STREAMS);
    config.bandwidth_limit_mbps = settings.bandwidth_limit_mbps;
    config.allow_untrusted = settings.allow_untrusted;
    let needs_restart =
        config.device_name != old.device_name || config.port != old.port || config.download_dir != old.download_dir;
    state.save_config(config.clone()).await?;

    let app_settings = AppSettings {
        language: settings.language.clone(),
        theme: settings.theme,
        close_to_tray: settings.close_to_tray,
        start_at_login: settings.start_at_login,
    };
    let to_save = app_settings.clone();
    tokio::task::spawn_blocking(move || to_save.save())
        .await
        .map_err(|error| CommandError::io(&error))?
        .map_err(|error| CommandError::io(&error))?;
    apply_start_at_login(&app, app_settings.start_at_login);
    *lock(&state.settings) = app_settings;

    if needs_restart {
        state.restart_service(&app).await?;
    } else {
        state.service().await.update_settings(config);
    }
    Ok(get_settings(state))
}

/// Replaces the tray menu labels with translated ones.
#[tauri::command]
pub fn set_tray_labels(menu: State<'_, TrayMenu>, labels: TrayLabels) -> CommandResult<()> {
    menu.set_labels(&labels).map_err(|error| CommandError::io(&error))
}

/// Applies a theme at once, to the window, its native parts and the web view's `prefers-color-scheme`. `None` follows
/// the operating system. Saving it is up to `update_settings`.
#[tauri::command]
pub fn set_theme(app: AppHandle, theme: Option<Theme>) {
    app.set_theme(theme);
}

/// Saves logs and settings to a zip file at `destination`.
#[tauri::command]
pub async fn export_diagnostics(destination: PathBuf) -> CommandResult<()> {
    tokio::task::spawn_blocking(move || {
        let (logs, config) = (paths::log_dir()?, paths::config_dir()?);
        diagnostics::export(&destination, &logs, &config).map_err(lanshare_core::Error::Io)
    })
    .await
    .map_err(|error| CommandError::io(&error))?
    .map_err(CommandError::from)
}

/// Registers or removes the login item. Failures are logged: the setting is still saved.
pub fn apply_start_at_login(app: &AppHandle, enabled: bool) {
    let autolaunch = app.autolaunch();
    let result = if enabled { autolaunch.enable() } else { autolaunch.disable() };
    if let Err(error) = result {
        tracing::warn!(%error, enabled, "could not change start at login");
    }
}
