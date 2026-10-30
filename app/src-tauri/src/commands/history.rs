//! The transfer history and opening folders.

use std::{path::PathBuf, sync::Arc};

use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

use crate::{
    error::{CommandError, CommandResult},
    history::HistoryEntry,
    state::AppState,
};

/// The most recent finished transfers, newest first.
#[tauri::command]
pub async fn get_history(state: State<'_, AppState>) -> CommandResult<Vec<HistoryEntry>> {
    let history = Arc::clone(&state.history);
    tokio::task::spawn_blocking(move || history.list())
        .await
        .map_err(|error| CommandError::io(&error))?
        .map_err(|error| CommandError::io(&error))
}

/// Deletes the history.
#[tauri::command]
pub async fn clear_history(state: State<'_, AppState>) -> CommandResult<()> {
    let history = Arc::clone(&state.history);
    tokio::task::spawn_blocking(move || history.clear())
        .await
        .map_err(|error| CommandError::io(&error))?
        .map_err(|error| CommandError::io(&error))
}

/// Shows a file in the system file manager, or opens a folder. Without a path, opens the download folder.
#[tauri::command]
pub async fn open_folder(app: AppHandle, state: State<'_, AppState>, path: Option<String>) -> CommandResult<()> {
    let opener = app.opener();
    let result = match path.map(PathBuf::from) {
        Some(path) if path.is_file() => opener.reveal_item_in_dir(&path),
        Some(path) => opener.open_path(path.display().to_string(), None::<&str>),
        None => opener.open_path(state.service().await.download_dir().display().to_string(), None::<&str>),
    };
    result.map_err(|error| CommandError::io(&error))
}
